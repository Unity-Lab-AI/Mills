//! Triggers (T-186: timers T-211, webhooks and events T-212, standing workflows T-213) with a clock of the test's own
//! (`Service::tick_at`) and the fake host. Checks R466 to R471 (bridge/test/regress.js maps the test names).
use pgm_workflow_engine_internal::fake::{FakeHost, Reply};
use pgm_workflow_engine_internal::service::{Answer, Caller, Directory, Service};
use pgm_workflow_engine_internal::{Engine, WorkflowHost};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MIN: u64 = 60_000;

struct Dir { host: Arc<FakeHost>, alerts: Mutex<Vec<(String, String)>> }
impl Directory for Dir {
    fn exists(&self, u: &str) -> bool { ["gee", "mills", "outsider"].contains(&u) }
    fn is_admin(&self, _u: &str) -> bool { false }
    fn scopes_of(&self, u: &str) -> Vec<String> {
        let mut v = vec![format!("me-{u}")];
        if u != "outsider" { v.push("team-core".into()); }
        v.push("company".into());
        v
    }
    fn host_for(&self, _u: &str) -> Arc<dyn WorkflowHost> { self.host.clone() }
    fn next_due(&self, ms: u64, recur: &str) -> Option<u64> {
        // the board's own repeats, as the hub gives them
        pgm_collab_internal::board::next_due(ms as i64, if recur == "nightly" { "daily" } else { recur }, 0).map(|x| x as u64)
    }
    fn alert(&self, user: &str, text: &str) { self.alerts.lock().unwrap().push((user.into(), text.into())); }
}

fn setup(name: &str) -> (Arc<Service>, Arc<Dir>, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("hw-trig-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let d = Arc::new(Dir { host: FakeHost::new(), alerts: Mutex::new(vec![]) });
    (Service::new(root.clone(), Engine::with_builtins(), d.clone()), d, root)
}

async fn call_as(s: &Service, user: &str, run_only: bool, method: &str, path: &str, q: &[(&str, &str)], body: Value) -> (u16, Value) {
    let query: HashMap<String, String> = q.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    match s.handle(&Caller { user: user.into(), run_only }, method, path, &query, &body).await { Answer::Json(st, v) => (st, v), Answer::Lines(_) => (200, Value::Null) }
}
async fn call(s: &Service, user: &str, method: &str, path: &str, q: &[(&str, &str)], body: Value) -> (u16, Value) { call_as(s, user, false, method, path, q, body).await }

async fn workflow(s: &Service, scope: &str, name: &str) -> String {
    let d = json!({ "schema": 1, "name": name, "nodes": [{ "id": "s", "kind": "start" }, { "id": "p", "kind": "prompt", "config": { "user": "{{inputs.prompt}} {{inputs.event.n|default:\"-\"}}" } }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "p.in" }, { "id": "e2", "from": "p.text", "to": "e.result" }] });
    let (_, saved) = call(s, "gee", "POST", "save", &[], json!({ "doc": d, "scope": scope })).await;
    assert_eq!(call(s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await.0, 200);
    saved["id"].as_str().unwrap().to_string()
}

async fn runs(s: &Service) -> Vec<Value> { call(s, "gee", "GET", "runs", &[], json!({})).await.1["runs"].as_array().cloned().unwrap_or_default() }
async fn trigger(s: &Service, id: &str) -> Value { call(s, "gee", "GET", "triggers", &[], json!({})).await.1["triggers"].as_array().unwrap().iter().find(|t| t["id"] == id).cloned().unwrap() }
async fn settle(s: &Service) { for _ in 0..100 { if runs(s).await.iter().all(|r| r["status"] != "running") { return; } tokio::time::sleep(Duration::from_millis(10)).await; } }

#[tokio::test]
async fn r466_timers_every_n_minutes_and_the_boards_repeats() {
    let (s, _d, _) = setup("r466");
    let wf = workflow(&s, "me-gee", "tick").await;
    let (st, t) = call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "timer", "timer": { "every": { "n": 15, "unit": "minutes" } }, "prompt": "on time", "title": "quarter-hourly" })).await;
    assert_eq!(st, 200, "{t}");
    let (id, next) = (t["id"].as_str().unwrap().to_string(), t["next"].as_u64().unwrap());
    s.tick_at(next - 1);
    assert!(runs(&s).await.is_empty(), "not yet");
    s.tick_at(next);
    s.tick_at(next + 1);
    settle(&s).await;
    let rs = runs(&s).await;
    assert_eq!(rs.len(), 1, "one run when due, not two");
    assert_eq!(rs[0]["result"], "ai:p:on time -");
    assert_eq!(trigger(&s, &id).await["next"], next + 15 * MIN);
    s.tick_at(next + 15 * MIN);
    settle(&s).await;
    assert_eq!(runs(&s).await.len(), 2, "and again a period later");
    assert_eq!(_d.host.log().iter().find(|e| e["type"] == "run_start").unwrap()["sender"]["kind"], "trigger", "a triggered run says so");
    // a monthly one moves on by the board's month-end aware repeat
    let start = 1_798_675_200_000u64; // 2026-12-31
    let (_, m) = call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "timer", "timer": { "recur": "monthly", "start": start } })).await;
    assert_eq!(m["next"], start);
    s.tick_at(start);
    let mt = trigger(&s, m["id"].as_str().unwrap()).await;
    assert_eq!(mt["next"].as_u64(), pgm_collab_internal::board::next_due(start as i64, "monthly", 0).map(|x| x as u64), "the board's own monthly repeat");
    assert_eq!(call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "timer", "timer": { "recur": "fortnightly" } })).await.0, 400);
    let (_, n) = call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "timer", "timer": { "recur": "nightly" } })).await;
    assert_eq!(n["next"].as_u64().unwrap() % 86_400_000, 2 * 3_600_000, "nightly: at 02:00 by default");
}

#[tokio::test]
async fn r467_no_double_run_after_a_restart_or_across_hubs() {
    let (a, d, root) = setup("r467");
    let wf = workflow(&a, "me-gee", "once").await;
    let (_, t) = call(&a, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "timer", "timer": { "every": { "n": 10, "unit": "minutes" } } })).await;
    let next = t["next"].as_u64().unwrap();
    a.tick_at(next);
    settle(&a).await;
    // the hub restarts: a new service on the same files, the same moment
    let b = Service::new(root.clone(), Engine::with_builtins(), d.clone());
    b.tick_at(next + 1);
    settle(&b).await;
    assert_eq!(runs(&b).await.len(), 1, "the next time was saved before the run: no second run after a restart");
    // two hubs on one store: the one holding the lease fires, the other waits for it to lapse
    a.tick_at(next + 10 * MIN);
    b.tick_at(next + 10 * MIN + 5_000);
    settle(&a).await;
    assert_eq!(runs(&a).await.len(), 2, "a fired the second time; b, inside a's lease, did not");
    b.tick_at(next + 20 * MIN + 40_000);
    settle(&b).await;
    assert_eq!(runs(&b).await.len(), 3, "a went quiet; b took the lease over and fires");
}

#[tokio::test]
async fn r468_a_missed_time_runs_once_or_is_skipped() {
    let (s, _d, _) = setup("r468");
    let wf = workflow(&s, "me-gee", "missed").await;
    for (policy, want) in [("once", 1usize), ("skip", 0)] {
        let before = runs(&s).await.len();
        let (_, t) = call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "timer", "timer": { "every": { "n": 10, "unit": "minutes" } }, "missed": policy })).await;
        let next = t["next"].as_u64().unwrap();
        s.tick_at(next + 35 * MIN); // the hub was down for three of its times
        settle(&s).await;
        assert_eq!(runs(&s).await.len() - before, want, "{policy}");
        let tr = trigger(&s, t["id"].as_str().unwrap()).await;
        assert_eq!(tr["next"], next + 40 * MIN, "{policy}: back on schedule");
        if policy == "skip" { assert_eq!(tr["runs"][0]["status"], "skipped"); }
    }
}

fn sig(secret: &str, body: &[u8]) -> String { pgm_core_internal::crypto::hmac_hex(secret.as_bytes(), body) }

#[tokio::test]
async fn r469_a_signed_forgejo_or_github_webhook_starts_a_run_with_its_payload() {
    let (s, d, _) = setup("r469");
    let wf = workflow(&s, "me-gee", "review").await;
    let (_, t) = call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "webhook", "webhook": { "events": ["pull_request"] }, "prompt": "Review PR {{event.pull_request.title}}" })).await;
    let (id, secret) = (t["id"].as_str().unwrap().to_string(), t["secret"].as_str().unwrap().to_string());
    assert_eq!(t["path"], format!("/hooks/hyperwork/{id}"));
    let body = br#"{"action": "opened", "pull_request": {"title": "Faster builds"}, "n": 7}"#;
    let h = |pairs: Vec<(&'static str, String)>| move |k: &str| pairs.iter().find(|(a, _)| a.eq_ignore_ascii_case(k)).map(|x| x.1.clone());
    let (st, r) = s.webhook(&id, &h(vec![("X-Forgejo-Event", "pull_request".into()), ("X-Forgejo-Signature", sig(&secret, body))]), body);
    assert_eq!(st, 202, "{r}");
    settle(&s).await;
    assert!(d.host.calls_of("p")[0].prompt.starts_with("Review PR Faster builds 7"), "the payload went in: {}", d.host.calls_of("p")[0].prompt);
    let (st, _) = s.webhook(&id, &h(vec![("X-GitHub-Event", "pull_request".into()), ("X-Hub-Signature-256", format!("sha256={}", sig(&secret, body)))]), body);
    assert_eq!(st, 202, "GitHub's signature too");
    assert_eq!(s.webhook(&id, &h(vec![("X-Forgejo-Event", "pull_request".into()), ("X-Forgejo-Signature", sig("wrong", body))]), body).0, 401);
    assert_eq!(s.webhook(&id, &h(vec![("X-Forgejo-Event", "pull_request".into())]), body).0, 401, "unsigned");
    let (st, ign) = s.webhook(&id, &h(vec![("X-Forgejo-Event", "issues".into()), ("X-Forgejo-Signature", sig(&secret, body))]), body);
    assert_eq!((st, ign["ignored"].as_str()), (202, Some("issues")), "an event it did not ask for starts nothing");
    settle(&s).await;
    assert_eq!(runs(&s).await.len(), 2);
    let listed = trigger(&s, &id).await;
    assert!(listed["webhook"].get("secret").is_none() && listed["webhook"]["hasSecret"] == true, "the secret is shown once, never listed");
}

#[tokio::test]
async fn r470_an_internal_event_starts_its_triggers() {
    let (s, _d, _) = setup("r470");
    let wf = workflow(&s, "me-gee", "on-deploy").await;
    call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "event", "event": { "name": "deploy-done" }, "prompt": "deployed {{event.version}}" })).await;
    assert!(s.event("something-else", &json!({})).is_empty());
    let started = s.event("deploy-done", &json!({ "version": "5.30.0" }));
    assert_eq!(started.len(), 1);
    settle(&s).await;
    assert_eq!(runs(&s).await[0]["result"], "ai:p:deployed 5.30.0 -");
    assert_eq!(call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "event", "event": { "name": "bad name!" } })).await.0, 400);
}

#[tokio::test]
async fn r471_standing_workflows_history_next_pause_resume_and_a_failure_alert() {
    let (s, d, _) = setup("r471");
    let wf = workflow(&s, "team-core", "board-manager").await;
    let (_, t) = call(&s, "gee", "POST", "trigger", &[], json!({ "workflow": wf, "kind": "timer", "timer": { "every": { "n": 1, "unit": "hours" } }, "title": "board manager" })).await;
    let (id, next) = (t["id"].as_str().unwrap().to_string(), t["next"].as_u64().unwrap());
    s.tick_at(next);
    settle(&s).await;
    let tr = trigger(&s, &id).await;
    assert_eq!((tr["runs"][0]["status"].as_str(), tr["next"].as_u64()), (Some("ok"), Some(next + 60 * MIN)), "its last run and its next time: {tr}");
    // the team sees it, only its owner changes it
    let (_, seen) = call(&s, "mills", "GET", "triggers", &[], json!({})).await;
    assert_eq!(seen["triggers"].as_array().unwrap().len(), 1);
    assert_eq!(call(&s, "mills", "POST", "trigger/pause", &[], json!({ "id": id })).await.0, 403);
    assert_eq!(call(&s, "outsider", "GET", "triggers", &[], json!({})).await.1["triggers"].as_array().unwrap().len(), 0);
    assert_eq!(call_as(&s, "gee", true, "POST", "trigger", &[], json!({ "workflow": wf, "kind": "event", "event": { "name": "x" } })).await.0, 403, "run-only clients set no triggers");
    // paused: nothing runs; resumed: on schedule from now
    assert_eq!(call(&s, "gee", "POST", "trigger/pause", &[], json!({ "id": id })).await.0, 200);
    s.tick_at(next + 60 * MIN);
    settle(&s).await;
    assert_eq!(runs(&s).await.len(), 1, "paused");
    let (_, r) = call(&s, "gee", "POST", "trigger/resume", &[], json!({ "id": id })).await;
    assert!(r["next"].as_u64().is_some());
    // a run that fails: its owner gets a chat alert
    d.host.reply("p", Reply::fail("model.broken"));
    s.tick_at(r["next"].as_u64().unwrap());
    settle(&s).await;
    let alerts = d.alerts.lock().unwrap().clone();
    assert!(alerts.iter().any(|(u, t)| u == "gee" && t.contains("board manager") && t.contains("failed")), "{alerts:?}");
    assert_eq!(trigger(&s, &id).await["runs"][0]["status"], "failed");
    assert_eq!(call(&s, "gee", "POST", "trigger/delete", &[], json!({ "id": id })).await.0, 200);
    assert!(call(&s, "gee", "GET", "triggers", &[], json!({})).await.1["triggers"].as_array().unwrap().is_empty());
}
