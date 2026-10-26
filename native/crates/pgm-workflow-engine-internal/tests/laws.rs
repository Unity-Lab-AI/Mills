//! Laws and saved prompts in a run (plans/hyper-workflows.md section 6, T-202: R426 to R429), with the fake host. The
//! store side (R425, R430, and R429's validation) is in tests/service.rs.
use pgm_workflow_engine_internal::fake::{FakeHost, Reply};
use pgm_workflow_engine_internal::model::shapes::{Group, Law};
use pgm_workflow_engine_internal::model::{doc, Document};
use pgm_workflow_engine_internal::{Engine, LawBook, RunInput, RunOptions, RunReport, WorkflowHost};
use serde_json::{json, Value};
use std::sync::Arc;

fn law(id: &str, extra: Value) -> Law {
    let mut v = json!({ "id": id, "name": id.trim_start_matches("law-"), "text": format!("RULE {id}"), "version": 3 });
    if let (Some(o), Value::Object(x)) = (v.as_object_mut(), extra) { for (k, val) in x { o.insert(k, val); } }
    serde_json::from_value(v).unwrap()
}
fn group(id: &str, members: &[&str]) -> Group { serde_json::from_value(json!({ "id": id, "name": id, "kind": "laws", "members": members })).unwrap() }
fn n(id: &str, kind: &str, extra: Value) -> Value { let mut v = json!({ "id": id, "kind": kind }); if let (Some(o), Value::Object(x)) = (v.as_object_mut(), extra) { for (k, val) in x { o.insert(k, val); } } v }
fn wf(nodes: Value, edges: &[(&str, &str)], extra: Value) -> Arc<Document> {
    let edges: Vec<Value> = edges.iter().enumerate().map(|(i, (f, t))| json!({ "id": format!("e{}", i + 1), "from": f, "to": t })).collect();
    let mut v = json!({ "schema": 1, "id": "wf-laws", "name": "laws", "nodes": nodes, "edges": edges });
    if let (Some(o), Value::Object(x)) = (v.as_object_mut(), extra) { for (k, val) in x { o.insert(k, val); } }
    Arc::new(doc::from_value(v).unwrap())
}
async fn go(d: &Arc<Document>, host: &Arc<FakeHost>) -> RunReport {
    let h: Arc<dyn WorkflowHost> = host.clone();
    Engine::with_builtins().start(d.clone(), RunInput::prompt("hi"), h, RunOptions::default()).await
}
/// The RULE lines a node's AI was told, in order.
fn told(host: &FakeHost, node: &str) -> Vec<String> {
    host.calls_of(node)[0].prompt.lines().filter_map(|l| l.split_once("RULE ").map(|x| x.1.to_string())).collect()
}

fn book() -> LawBook {
    LawBook {
        laws: ["law-scope", "law-wf", "law-dup", "law-big", "law-small", "law-node", "law-other"].iter().map(|i| law(i, json!({}))).collect(),
        groups: vec![group("grp-g", &["law-wf", "law-dup"])],
        defaults: vec!["law-scope".into()],
    }
}

fn three(node_a: Value, node_b: Value) -> Arc<Document> {
    wf(json!([n("s", "start", json!({})), n("a", "prompt", node_a), n("b", "prompt", node_b), n("c", "prompt", json!({})), n("j", "join", json!({})), n("e", "end", json!({}))]),
        &[("s.prompt", "a.in"), ("s.prompt", "b.in"), ("s.prompt", "c.in"), ("a.text", "j.lines"), ("b.text", "j.lines"), ("c.text", "j.lines"), ("j.result", "e.result")],
        json!({ "defaults": { "laws": ["law-wf", "grp-g"] }, "sections": [
            { "id": "big", "nodes": ["a", "b"], "laws": ["law-big"] },
            { "id": "small", "nodes": ["a"], "laws": ["law-small"] } ] }))
}

#[tokio::test]
async fn r426_the_right_laws_reach_the_right_node_and_nowhere_else() {
    let host = FakeHost::new();
    host.laws(book());
    let r = go(&three(json!({ "laws": ["law-node", "law-wf"] }), json!({})), &host).await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(told(&host, "a"), ["law-scope", "law-wf", "law-dup", "law-big", "law-small", "law-node"], "scope, workflow (a group expanded), sections outer to inner, node; each once");
    assert_eq!(told(&host, "b"), ["law-scope", "law-wf", "law-dup", "law-big"], "b is only in the big section");
    assert_eq!(told(&host, "c"), ["law-scope", "law-wf", "law-dup"], "c is in no section");
    assert!(host.all_calls().iter().all(|c| !c.prompt.contains("law-other")), "a law nobody applied reaches no one");
    let start_a = r.log.iter().find(|e| e["type"] == "node_start" && e["key"] == "a").unwrap();
    assert_eq!(start_a["laws"].as_array().unwrap().iter().map(|l| l["id"].as_str().unwrap()).collect::<Vec<_>>(), ["law-scope", "law-wf", "law-dup", "law-big", "law-small", "law-node"]);
    assert_eq!(start_a["laws"][0]["v"], 3, "the log names each law's version");
    assert!(host.calls_of("a")[0].prompt.starts_with("Laws (follow every one of them):"), "they go into the system part, under a heading");
    let start_s = r.log.iter().find(|e| e["type"] == "node_start" && e["key"] == "s").unwrap();
    assert!(start_s.get("laws").is_none(), "a node with no AI call is not told laws");
    // no laws in the scope: nothing is added
    let plain = FakeHost::new();
    go(&three(json!({ "laws": ["law-node"] }), json!({})), &plain).await;
    assert!(!plain.calls_of("a")[0].prompt.contains("Laws"));
}

#[tokio::test]
async fn r427_a_lower_level_adds_and_cannot_remove_a_locked_law() {
    let host = FakeHost::new();
    let mut b = book();
    b.laws[0] = law("law-scope", json!({ "lockable": false }));
    host.laws(b);
    let r = go(&three(json!({}), json!({ "laws": ["-law-wf", "-law-scope", "law-other"] })), &host).await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(told(&host, "b"), ["law-scope", "law-dup", "law-big", "law-other"], "law-wf removed, law-scope (lockable: false) stays, law-other added");
    assert_eq!(told(&host, "a"), ["law-scope", "law-wf", "law-dup", "law-big", "law-small"], "the removal is b's alone");
}

fn checked(on_fail: &str, check: Value) -> (Arc<FakeHost>, Arc<Document>) {
    let host = FakeHost::new();
    let mut c = check;
    c["onFail"] = json!(on_fail);
    host.laws(LawBook { laws: vec![law("law-ok", json!({ "check": c }))], groups: vec![], defaults: vec![] });
    let d = wf(json!([n("s", "start", json!({})), n("p", "prompt", json!({ "laws": ["law-ok"] })), n("e", "end", json!({}))]), &[("s.prompt", "p.in"), ("p.text", "e.result")], json!({}));
    (host, d)
}

#[tokio::test]
async fn r428_a_failed_law_check_follows_its_on_fail() {
    let regex = json!({ "kind": "regex", "pattern": "^OK" });
    let (host, d) = checked("retry", regex.clone());
    host.replies("p", vec![Reply::text("nope"), Reply::text("OK fine")]);
    let r = go(&d, &host).await;
    assert_eq!(r.result, Some(json!("OK fine")), "{r:?}");
    assert!(host.calls_of("p")[1].prompt.contains("breaks the law"), "asked again, told which law it broke");
    let (host, d) = checked("retry", regex.clone());
    host.replies("p", vec![Reply::text("nope"), Reply::text("still nope")]);
    assert_eq!(go(&d, &host).await.error.unwrap()["code"], "engine.law-check", "retried once, then an error");
    let (host, d) = checked("error", regex.clone());
    host.reply("p", Reply::text("nope"));
    assert_eq!(go(&d, &host).await.error.unwrap()["code"], "engine.law-check");
    let (host, d) = checked("warn", regex);
    host.reply("p", Reply::text("nope"));
    let r = go(&d, &host).await;
    assert_eq!(r.result, Some(json!("nope")), "a warn law lets it through");
    let end = r.log.iter().find(|e| e["type"] == "node_end" && e["key"] == "p").unwrap();
    assert!(end["warnings"][0].as_str().unwrap().contains("law-ok"), "{end}");
    let (host, d) = checked("error", json!({ "kind": "expr", "expr": "len(value) < 10" }));
    host.reply("p", Reply::text("a far too long answer"));
    assert_eq!(go(&d, &host).await.error.unwrap()["code"], "engine.law-check", "an expr check");
    let (host, d) = checked("error", json!({ "kind": "jev", "question": "Is it polite?" }));
    host.reply("p", Reply::text("go away")).reply("p#law-law-ok", Reply::text(r#"{"choice": "no"}"#));
    assert_eq!(go(&d, &host).await.error.unwrap()["code"], "engine.law-check", "a jev check asks the decision model");
    let (host, d) = checked("error", json!({ "kind": "jev", "question": "Is it polite?" }));
    host.reply("p", Reply::text("thank you")).reply("p#law-law-ok", Reply::text(r#"{"choice": "yes"}"#));
    assert_eq!(go(&d, &host).await.status, "ok");
}

#[tokio::test]
async fn r429_a_saved_prompt_latest_follows_edits_and_a_pinned_one_does_not() {
    let host = FakeHost::new();
    host.prompt("style", "the newest style").prompt("style@1", "the first style");
    let d = wf(json!([n("s", "start", json!({})), n("latest", "prompt", json!({ "config": { "system": "{{prompt:style}}" } })), n("pinned", "prompt", json!({ "config": { "system": "{{prompt:style@1}}" } })),
            n("j", "join", json!({})), n("e", "end", json!({}))]),
        &[("s.prompt", "latest.in"), ("s.prompt", "pinned.in"), ("latest.text", "j.lines"), ("pinned.text", "j.lines"), ("j.result", "e.result")], json!({}));
    let r = go(&d, &host).await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert!(host.calls_of("latest")[0].prompt.starts_with("the newest style"));
    assert!(host.calls_of("pinned")[0].prompt.starts_with("the first style"));
}
