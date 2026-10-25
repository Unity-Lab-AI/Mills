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

#[tokio::test]
async fn r460_a_hyperwork_line_runs_or_names_the_nearest() {
    let root = tmp("r460");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    published(&s, "gee", "me-gee", "release-notes").await;
    published(&s, "gee", "team-core", "team-review").await;
    let (st, r) = call(&s, "gee", "POST", "command", &[], json!({ "line": "/hyperwork release-notes write them --project VibeRoller", "project": "Other" })).await;
    assert_eq!(st, 202, "{r}");
    let m = until_done(&s, "gee", r["run"].as_str().unwrap()).await;
    assert_eq!(m["result"], "ai:p:write them");
    let (_, lines) = call(&s, "gee", "GET", "log", &[("id", r["run"].as_str().unwrap())], json!({})).await;
    assert_eq!(lines[0]["project"], "VibeRoller", "the line's --project wins over the product's");
    let (st, miss) = call(&s, "gee", "POST", "command", &[], json!({ "line": "/hyperwork release x" })).await;
    assert_eq!(st, 404, "a name that matches nothing runs nothing");
    assert_eq!(miss["nearest"], json!(["release-notes"]));
    assert_eq!(call(&s, "gee", "POST", "command", &[], json!({ "line": "/hyperwork release-notes --on moon x" })).await.0, 400);
    assert_eq!(call(&s, "mills", "POST", "command", &[], json!({ "line": "/hyperwork release-notes x" })).await.0, 404, "someone else's personal workflow is not theirs to run");
    assert_eq!(call(&s, "mills", "POST", "command", &[], json!({ "line": "/hw team-review look" })).await.0, 202, "the short form, a team workflow");
}

// ---------------------------------------------------------------- T-202: laws, saved prompts, groups in the store

async fn shelve(s: &Service, user: &str, what: &str, item: Value, scope: Option<&str>) -> (u16, Value) {
    let mut body = json!({ "item": item });
    if let Some(sc) = scope { body["scope"] = json!(sc); }
    call(s, user, "POST", what, &[], body).await
}

#[tokio::test]
async fn r425_laws_prompts_and_groups_saved_grouped_listed_deleted_versioned() {
    let root = tmp("r425");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let (st, tone) = shelve(&s, "gee", "law", json!({ "name": "tone", "text": "Answer kindly." }), None).await;
    assert_eq!(st, 200, "{tone}");
    let tone = tone["id"].as_str().unwrap().to_string();
    assert!(tone.starts_with("law-"));
    let cite = shelve(&s, "gee", "law", json!({ "name": "cite", "text": "Cite file and line." }), None).await.1["id"].as_str().unwrap().to_string();
    let alone = shelve(&s, "gee", "law", json!({ "name": "alone", "text": "A standalone law." }), None).await.1["id"].as_str().unwrap().to_string();
    let pr = shelve(&s, "gee", "prompt", json!({ "name": "release-style", "text": "Write notes for {{audience}}.", "slots": [{ "name": "audience", "default": "the team" }] }), None).await.1["id"].as_str().unwrap().to_string();
    let g1 = shelve(&s, "gee", "group", json!({ "name": "style", "kind": "laws", "members": [tone, cite] }), None).await.1["id"].as_str().unwrap().to_string();
    let (st, g2) = shelve(&s, "gee", "group", json!({ "name": "all", "kind": "mixed", "members": [tone, pr] }), None).await;
    assert_eq!(st, 200, "a law can be in two groups at once: {g2}");
    let (_, laws) = call(&s, "gee", "GET", "laws", &[], json!({})).await;
    assert_eq!(laws["items"].as_array().unwrap().len(), 3);
    assert_eq!(call(&s, "gee", "GET", "groups", &[], json!({})).await.1["items"].as_array().unwrap().len(), 2);
    // a second save is a second version, the first stays readable
    let (_, v2) = shelve(&s, "gee", "law", json!({ "id": tone, "name": "tone", "text": "Answer kindly and briefly." }), None).await;
    assert_eq!(v2["version"], 2);
    assert!(root.join("me-gee").join("laws").join(format!("{tone}.v1.json")).exists() && root.join("me-gee").join("laws").join(format!("{tone}.v2.json")).exists());
    assert_eq!(call(&s, "gee", "GET", "laws", &[("id", &tone), ("version", "1")], json!({})).await.1["item"]["text"], "Answer kindly.");
    // deleting a group keeps its members; a deleted law is gone from the list
    assert_eq!(call(&s, "gee", "POST", "group/delete", &[], json!({ "id": g1 })).await.0, 200);
    assert_eq!(call(&s, "gee", "GET", "laws", &[], json!({})).await.1["items"].as_array().unwrap().len(), 3, "the members stay");
    assert_eq!(call(&s, "gee", "POST", "law/delete", &[], json!({ "id": alone })).await.0, 200);
    assert_eq!(call(&s, "gee", "GET", "laws", &[], json!({})).await.1["items"].as_array().unwrap().len(), 2);
    // a bad one is refused with its problems; the team's laws are its admins' (and lawEditors')
    let (st, bad) = shelve(&s, "gee", "law", json!({ "name": "empty", "text": " " }), None).await;
    assert_eq!(st, 400, "{bad}");
    assert_eq!(shelve(&s, "mills", "law", json!({ "name": "t", "text": "team rule" }), Some("team-core")).await.0, 403);
    assert_eq!(shelve(&s, "sponge", "law", json!({ "name": "t", "text": "team rule" }), Some("team-core")).await.0, 200);
    assert_eq!(shelve(&s, "outsider", "law", json!({ "name": "t", "text": "x" }), Some("team-core")).await.0, 403);
    assert_eq!(call_as(&s, "gee", true, "POST", "law", &[], json!({ "item": { "name": "x", "text": "y" } })).await.0, 403, "run-only clients change nothing");
    assert_eq!(call(&s, "mills", "GET", "laws", &[("scope", "team-core")], json!({})).await.1["items"].as_array().unwrap().len(), 1, "team members see the team's laws");
}

#[tokio::test]
async fn r426_store_laws_reach_the_right_node_of_a_hub_run() {
    let root = tmp("r426s");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let team = shelve(&s, "sponge", "law", json!({ "name": "team", "text": "RULE team" }), Some("team-core")).await.1["id"].as_str().unwrap().to_string();
    let only = shelve(&s, "sponge", "law", json!({ "name": "only", "text": "RULE only-a" }), Some("team-core")).await.1["id"].as_str().unwrap().to_string();
    let _unused = shelve(&s, "sponge", "law", json!({ "name": "unused", "text": "RULE unused" }), Some("team-core")).await;
    call(&s, "sponge", "POST", "policy", &[], json!({ "scope": "team-core", "policy": { "laws": [team] } })).await;
    let d = json!({ "schema": 1, "name": "two", "nodes": [{ "id": "s", "kind": "start" }, { "id": "a", "kind": "prompt", "laws": [only] }, { "id": "b", "kind": "prompt" }, { "id": "j", "kind": "join" }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "a.in" }, { "id": "e2", "from": "s.prompt", "to": "b.in" }, { "id": "e3", "from": "a.text", "to": "j.lines" }, { "id": "e4", "from": "b.text", "to": "j.lines" }, { "id": "e5", "from": "j.result", "to": "e.result" }] });
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": d, "scope": "team-core" })).await;
    assert_eq!(call(&s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await.0, 200);
    let (_, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "two", "prompt": "go" })).await;
    let m = until_done(&s, "gee", r["run"].as_str().unwrap()).await;
    assert_eq!(m["status"], "ok", "{m}");
    let a = host.calls_of("a")[0].prompt.clone();
    let b = host.calls_of("b")[0].prompt.clone();
    assert!(a.contains("RULE team") && a.contains("RULE only-a"), "{a}");
    assert!(b.contains("RULE team") && !b.contains("RULE only-a"), "the node's own law reaches it and nowhere else: {b}");
    assert!(!a.contains("RULE unused") && !b.contains("RULE unused"));
}

#[tokio::test]
async fn r429_a_saved_prompt_from_the_store_latest_and_pinned_and_an_unknown_one_is_refused() {
    let root = tmp("r429s");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let doc = |system: &str| json!({ "schema": 1, "name": "notes", "nodes": [{ "id": "s", "kind": "start" }, { "id": "p", "kind": "prompt", "config": { "system": system } }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "p.in" }, { "id": "e2", "from": "p.text", "to": "e.result" }] });
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": doc("{{prompt:release-style}}") })).await;
    assert!(saved["problems"].as_array().unwrap().iter().any(|p| p["code"] == "prompt.missing"), "an unknown prompt is a validation error: {saved}");
    assert_eq!(call(&s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await.0, 422);
    let pr = shelve(&s, "gee", "prompt", json!({ "name": "release-style", "text": "First: notes for {{audience}}.", "slots": [{ "name": "audience", "default": "the team" }] }), None).await.1["id"].as_str().unwrap().to_string();
    shelve(&s, "gee", "prompt", json!({ "id": pr, "name": "release-style", "text": "Second: notes for {{audience}}.", "slots": [{ "name": "audience", "default": "the team" }] }), None).await;
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "id": saved["id"], "doc": doc("{{prompt:release-style}} / {{prompt:release-style@1}}") })).await;
    assert!(!saved["problems"].as_array().unwrap().iter().any(|p| p["level"] == "error"), "{saved}");
    assert_eq!(call(&s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await.0, 200);
    let (_, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "notes", "prompt": "go" })).await;
    until_done(&s, "gee", r["run"].as_str().unwrap()).await;
    assert!(host.calls_of("p")[0].prompt.starts_with("Second: notes for the team. / First: notes for the team."), "latest follows the edit, @1 stays: {}", host.calls_of("p")[0].prompt);
}

#[tokio::test]
async fn r430_an_export_carries_its_laws_and_prompts_and_an_import_checks_them() {
    let root = tmp("r430");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let l1 = shelve(&s, "gee", "law", json!({ "name": "a", "text": "Rule A." }), None).await.1["id"].as_str().unwrap().to_string();
    let l2 = shelve(&s, "gee", "law", json!({ "name": "b", "text": "Rule B." }), None).await.1["id"].as_str().unwrap().to_string();
    shelve(&s, "gee", "law", json!({ "name": "c", "text": "Not used." }), None).await;
    let g = shelve(&s, "gee", "group", json!({ "name": "ab", "kind": "laws", "members": [l1, l2] }), None).await.1["id"].as_str().unwrap().to_string();
    shelve(&s, "gee", "prompt", json!({ "name": "style", "text": "Be brief." }), None).await;
    let d = json!({ "schema": 1, "name": "carry", "defaults": { "laws": [g] }, "nodes": [{ "id": "s", "kind": "start" }, { "id": "p", "kind": "prompt", "config": { "system": "{{prompt:style}}" } }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "p.in" }, { "id": "e2", "from": "p.text", "to": "e.result" }] });
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": d })).await;
    call(&s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await;
    let (st, ex) = call(&s, "gee", "POST", "export", &[], json!({ "id": saved["id"], "bundle": true })).await;
    assert_eq!(st, 200, "{ex}");
    let b = &ex["bundle"];
    assert_eq!((b["laws"].as_array().unwrap().len(), b["groups"].as_array().unwrap().len(), b["prompts"].as_array().unwrap().len()), (2, 1, 1), "the group's two laws, the group, the prompt; not the unused law: {b}");
    let (st, im) = call(&s, "sponge", "POST", "import", &[], json!({ "bundle": b, "scope": "me-sponge" })).await;
    assert_eq!(st, 200, "{im}");
    assert!(!im["problems"].as_array().unwrap().iter().any(|p| p["level"] == "error"), "everything it uses came with it: {im}");
    assert_eq!(call(&s, "sponge", "GET", "laws", &[], json!({})).await.1["items"].as_array().unwrap().len(), 2);
    let mut broken = b.clone();
    broken["laws"][0]["text"] = json!("");
    assert_eq!(call(&s, "sponge", "POST", "import", &[], json!({ "bundle": broken, "scope": "me-sponge" })).await.0, 400, "a bad law in the bundle is refused");
}

// ---------------------------------------------------------------- T-203: custom node kinds in the store

/// A transform kind: `out` is its `text` input in capitals (tested on publish).
fn upper(kind: &str, version: &str) -> Value {
    json!({ "schema": 1, "kind": kind, "version": version, "title": "Shout", "ports": { "in": [{ "name": "text", "type": "json" }], "out": [{ "name": "out", "type": "json" }, { "name": "error", "type": "error" }] },
        "config": { "type": "object" }, "impl": { "type": "script", "code": "out = upper(text)", "outputs": [{ "name": "out", "type": "json" }] },
        "tests": [{ "name": "shouts", "in": { "text": "hi" }, "expect": { "out": "HI" } }] })
}
fn using(kind: &str, major: u32) -> Value {
    json!({ "schema": 1, "name": format!("use-{}", kind.replace('/', "-")), "nodes": [{ "id": "s", "kind": "start" }, { "id": "k", "kind": kind, "kindVersion": major }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "k.text" }, { "id": "e2", "from": "k.out", "to": "e.result" }] })
}
async fn publish_wf(s: &Service, user: &str, d: Value, scope: &str) -> String {
    let (st, saved) = call(s, user, "POST", "save", &[], json!({ "doc": d, "scope": scope })).await;
    assert_eq!(st, 200, "{saved}");
    let (st, p) = call(s, user, "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await;
    assert_eq!(st, 200, "{p}");
    saved["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn r431_a_new_node_from_a_selection_is_saved_in_the_personal_scope_and_used() {
    let root = tmp("r431");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let chain = json!({ "schema": 1, "name": "two-step", "nodes": [{ "id": "s", "kind": "start" }, { "id": "p1", "kind": "prompt" }, { "id": "p2", "kind": "prompt" }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "p1.in" }, { "id": "e2", "from": "p1.text", "to": "p2.in" }, { "id": "e3", "from": "p2.text", "to": "e.result" }] });
    let (st, sel) = call(&s, "gee", "POST", "selection", &[], json!({ "doc": chain, "nodes": ["p1", "p2"], "name": "double", "title": "Ask twice" })).await;
    assert_eq!(st, 200, "{sel}");
    assert_eq!(sel["kind"], "gee/double");
    let (_, kinds) = call(&s, "gee", "GET", "kinds", &[], json!({})).await;
    let mine = kinds["kinds"].as_array().unwrap().iter().find(|k| k["kind"] == "gee/double").expect("listed");
    assert_eq!((mine["scope"].as_str(), mine["trust"].as_str()), (Some("me-gee"), Some("author-only")));
    assert_eq!(mine["ports"]["in"][0]["name"], "p1_in", "its ports are those of the selection");
    let mut d = sel["doc"].clone();
    d["name"] = json!("two-step");
    publish_wf(&s, "gee", d, "me-gee").await;
    let (st, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "two-step", "prompt": "hi" })).await;
    assert_eq!(st, 202, "{r}");
    let m = until_done(&s, "gee", r["run"].as_str().unwrap()).await;
    assert_eq!(m["result"], "ai:p2:ai:p1:hi", "{m}");
    let (_, other) = call(&s, "mills", "GET", "kinds", &[], json!({})).await;
    assert!(!other["kinds"].as_array().unwrap().iter().any(|k| k["kind"] == "gee/double"), "a personal kind is its owner's");
}

#[tokio::test]
async fn r433_publishing_a_kind_runs_its_tests() {
    let root = tmp("r433s");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let mut bad = upper("gee/shout", "1.0.0");
    bad["impl"]["code"] = json!("out = lower(text)");
    let (st, r) = call(&s, "gee", "POST", "kind", &[], json!({ "manifest": bad })).await;
    assert_eq!(st, 422, "{r}");
    assert!(r["failures"][0].as_str().unwrap().contains("shouts"));
    assert_eq!(call(&s, "gee", "POST", "kind", &[], json!({ "manifest": upper("gee/shout", "1.0.0") })).await.0, 200);
    assert_eq!(call(&s, "gee", "POST", "kind", &[], json!({ "manifest": upper("gee/shout", "1.0.0") })).await.0, 409, "a version is published once");
    assert_eq!(call(&s, "gee", "POST", "kind", &[], json!({ "manifest": upper("prompt", "9.0.0") })).await.0, 400, "a built-in cannot be replaced");
}

#[tokio::test]
async fn r434_a_pin_stays_on_its_major_and_a_changed_file_is_refused() {
    let root = tmp("r434s");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    call(&s, "gee", "POST", "kind", &[], json!({ "manifest": upper("gee/shout", "1.0.0") })).await;
    let mut two = upper("gee/shout", "2.0.0");
    two["impl"]["code"] = json!("out = 'v2: ' + upper(text)");
    two["tests"][0]["expect"]["out"] = json!("v2: HI");
    assert_eq!(call(&s, "gee", "POST", "kind", &[], json!({ "manifest": two })).await.0, 200);
    publish_wf(&s, "gee", using("gee/shout", 1), "me-gee").await;
    let (_, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "use-gee-shout", "prompt": "hi" })).await;
    assert_eq!(until_done(&s, "gee", r["run"].as_str().unwrap()).await["result"], "HI", "pinned to 1: 2.0.0 is never picked up");
    // someone changes the stored 1.0.0 by hand: its hash no longer matches, the run is refused before it starts
    let f = root.join("me-gee").join("kinds").join("gee__shout").join("1.0.0.json");
    let mut rec: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
    rec["manifest"]["impl"]["code"] = json!("out = 'evil'");
    std::fs::write(&f, rec.to_string()).unwrap();
    let (st, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "use-gee-shout", "prompt": "hi" })).await;
    assert_eq!(st, 422, "{r}");
    assert!(r["problems"].as_array().unwrap().iter().any(|p| p["code"] == "engine.kind-hash"), "{r}");
}

#[tokio::test]
async fn r435_trust_author_only_reviewed_two_reviewers_blocked() {
    let root = tmp("r435");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    assert_eq!(call(&s, "gee", "POST", "kind", &[], json!({ "manifest": upper("gee/shout", "1.0.0"), "scope": "team-core" })).await.0, 200);
    publish_wf(&s, "gee", using("gee/shout", 1), "team-core").await;
    let (st, r) = call(&s, "mills", "POST", "run", &[], json!({ "name": "use-gee-shout", "prompt": "hi" })).await;
    assert_eq!(st, 422, "author-only: only its author runs it: {r}");
    assert_eq!(r["problems"][0]["code"], "engine.kind-untrusted");
    assert_eq!(call(&s, "gee", "POST", "run", &[], json!({ "name": "use-gee-shout", "prompt": "hi" })).await.0, 202, "its author does");
    assert_eq!(call(&s, "gee", "POST", "kind/review", &[], json!({ "kind": "gee/shout" })).await.0, 403, "not by its author");
    assert_eq!(call(&s, "mills", "POST", "kind/review", &[], json!({ "kind": "gee/shout" })).await.0, 403, "only an admin or a lawEditor reviews");
    assert_eq!(call(&s, "sponge", "POST", "kind/review", &[], json!({ "kind": "gee/shout" })).await.1["trust"], "reviewed");
    let (st, r) = call(&s, "mills", "POST", "run", &[], json!({ "name": "use-gee-shout", "prompt": "hi" })).await;
    assert_eq!(st, 202, "reviewed: the team runs it: {r}");
    assert_eq!(until_done(&s, "mills", r["run"].as_str().unwrap()).await["result"], "HI");
    // a sandboxed program needs two reviewers
    let prog = json!({ "schema": 1, "kind": "gee/prog", "version": "1.0.0", "ports": { "in": [], "out": [] }, "impl": { "type": "program", "cmd": "x" } });
    assert_eq!(call(&s, "gee", "POST", "kind", &[], json!({ "manifest": prog, "scope": "team-core" })).await.0, 200);
    assert_eq!(call(&s, "sponge", "POST", "kind/review", &[], json!({ "kind": "gee/prog" })).await.1["trust"], "author-only", "one review is not enough");
    call(&s, "sponge", "POST", "policy", &[], json!({ "scope": "team-core", "policy": { "lawEditors": ["mills"] } })).await;
    assert_eq!(call(&s, "mills", "POST", "kind/review", &[], json!({ "kind": "gee/prog" })).await.1["trust"], "reviewed", "the second makes it");
    // blocked: neither added nor run
    assert_eq!(call(&s, "mills", "POST", "kind/block", &[], json!({ "kind": "gee/shout", "scope": "team-core" })).await.0, 403);
    assert_eq!(call(&s, "sponge", "POST", "kind/block", &[], json!({ "kind": "gee/shout", "scope": "team-core" })).await.0, 200);
    let (st, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "use-gee-shout", "prompt": "hi" })).await;
    assert_eq!((st, r["problems"][0]["code"].as_str()), (422, Some("engine.kind-blocked")), "{r}");
    let mut again = using("gee/shout", 1);
    again["name"] = json!("another");
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": again, "scope": "team-core" })).await;
    assert!(saved["problems"].as_array().unwrap().iter().any(|p| p["code"] == "engine.kind-blocked"), "and cannot be added: {saved}");
}

#[tokio::test]
async fn r436_a_team_kind_fetched_by_another_runner_is_hash_checked() {
    let root = tmp("r436");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    call(&s, "gee", "POST", "kind", &[], json!({ "manifest": upper("gee/shout", "1.0.0"), "scope": "team-core" })).await;
    call(&s, "sponge", "POST", "kind/review", &[], json!({ "kind": "gee/shout" })).await;
    // mills, on another runner (jcode on a laptop): reads the team's kinds, installs the one it needs, checks the hash
    let (_, kinds) = call_as(&s, "mills", true, "GET", "kinds", &[("scope", "team-core")], json!({})).await;
    let k = kinds["kinds"].as_array().unwrap().iter().find(|k| k["kind"] == "gee/shout").unwrap().clone();
    assert_eq!(k["runnable"], true);
    let hash = k["hash"].as_str().unwrap().to_string();
    let mut m = k.clone();
    for f in ["trust", "hash", "author", "scope", "reviewers", "at", "runnable"] { m.as_object_mut().unwrap().remove(f); }
    let manifest: pgm_workflow_engine_internal::model::KindManifest = serde_json::from_value(m).unwrap();
    let mut local = Engine::with_builtins();
    local.install(manifest.clone(), Some(&hash)).unwrap();
    let d = pgm_workflow_engine_internal::model::doc::from_value({ let mut v = using("gee/shout", 1); v["id"] = json!("wf-local"); v }).unwrap();
    let h: Arc<dyn WorkflowHost> = FakeHost::new();
    let r = local.start(Arc::new(d), pgm_workflow_engine_internal::RunInput::prompt("from the laptop"), h, pgm_workflow_engine_internal::RunOptions::default()).await;
    assert_eq!(r.result, Some(json!("FROM THE LAPTOP")), "{r:?}");
    let mut tampered = manifest;
    tampered.title = Some("not what was published".into());
    assert!(Engine::with_builtins().install(tampered, Some(&hash)).is_err(), "a hash that does not match is refused");
}

// ---------------------------------------------------------------- T-206: the AI side chat

fn side(text: &str, patch: Value) -> Reply { Reply::text(&json!({ "text": text, "patch": patch }).to_string()) }

#[tokio::test]
async fn r454_r455_the_side_chat_answers_and_proposes_a_patch_and_writes_nothing() {
    let root = tmp("r454");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let id = published(&s, "gee", "me-gee", "notes").await;
    let (_, before) = call(&s, "gee", "GET", "get", &[("id", &id)], json!({})).await;
    host.reply("side-chat", side("Add a summary before the end.", json!([
        { "type": "add", "kind": "summarise", "id": "sum", "x": 300, "y": 0 },
        { "type": "disconnect", "from": "p.text", "to": "e.result" },
        { "type": "connect", "from": "p.text", "to": "sum.text" },
        { "type": "connect", "from": "sum.summary", "to": "e.result" } ])));
    let (st, a) = call(&s, "gee", "POST", "ask", &[], json!({ "id": id, "message": "make it shorter", "selection": ["p"] })).await;
    assert_eq!(st, 200, "{a}");
    assert_eq!(a["text"], "Add a summary before the end.");
    assert_eq!(a["patch"].as_array().unwrap().len(), 4);
    assert!(a["problems"].as_array().unwrap().is_empty(), "the patch fits: {a}");
    let (_, after) = call(&s, "gee", "GET", "get", &[("id", &id)], json!({})).await;
    assert_eq!(before["doc"], after["doc"], "asking changed nothing: the person accepts the patch into their draft");
    assert_eq!(call(&s, "gee", "GET", "versions", &[("id", &id)], json!({})).await.1["versions"].as_array().unwrap().len(), 1, "nothing published");
    assert!(call(&s, "gee", "GET", "runs", &[], json!({})).await.1["runs"].as_array().unwrap().is_empty(), "nothing run");
    assert!(host.calls_of("side-chat")[0].prompt.contains("make it shorter") && host.calls_of("side-chat")[0].prompt.contains("Selected: [\"p\"]"));
}

#[tokio::test]
async fn r456_r457_only_the_editors_commands_and_kinds_that_exist() {
    let root = tmp("r456");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let id = published(&s, "gee", "me-gee", "notes").await;
    host.reply("side-chat", side("Published it for you.", json!([{ "type": "publish" }])));
    let (_, a) = call(&s, "gee", "POST", "ask", &[], json!({ "id": id, "message": "publish it" })).await;
    assert!(a["problems"][0]["message"].as_str().unwrap().contains("not a patch command"), "a patch only draws: {a}");
    assert_eq!(call(&s, "gee", "GET", "versions", &[("id", &id)], json!({})).await.1["versions"].as_array().unwrap().len(), 1);
    let prompt = host.calls_of("side-chat")[0].prompt.clone();
    assert!(prompt.contains("\"kind\":\"jev\"") && prompt.contains("\"kind\":\"prompt\""), "it is told the kinds it may use");
    host.reply("side-chat", side("Added a node.", json!([{ "type": "add", "kind": "acme/nothing", "id": "x" }])));
    let (_, a) = call(&s, "gee", "POST", "ask", &[], json!({ "id": id, "message": "add something" })).await;
    assert!(a["problems"].as_array().unwrap().iter().any(|p| p["code"] == "kind.unknown"), "a patch that breaks validation comes with its problems: {a}");
    assert_eq!(call_as(&s, "gee", true, "POST", "ask", &[], json!({ "id": id, "message": "x" })).await.0, 403, "designing is not for run-only clients");
    assert_eq!(call(&s, "mills", "POST", "ask", &[], json!({ "id": id, "message": "x" })).await.0, 404, "only about workflows they may see");
}

#[tokio::test]
async fn r458_no_ai_says_so_and_the_editor_still_works() {
    let root = tmp("r458");
    let host = FakeHost::new();
    host.reply("side-chat", Reply::fail("model.unreachable"));
    let s = svc(&root, &dir(&host));
    let (st, a) = call(&s, "gee", "POST", "ask", &[], json!({ "doc": notes_doc("draft"), "message": "what does it do?" })).await;
    assert_eq!((st, a["noAi"].as_bool()), (200, Some(true)), "{a}");
    assert!(a["text"].as_str().unwrap().contains("No AI"));
    assert_eq!(call(&s, "gee", "POST", "save", &[], json!({ "doc": notes_doc("draft") })).await.0, 200, "the editor still saves");
}
