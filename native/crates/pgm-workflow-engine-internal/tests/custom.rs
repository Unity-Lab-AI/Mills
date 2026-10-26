//! Custom node kinds in the engine (plans/hyper-workflows.md section 11, T-203: R431 to R436, the engine's part); the
//! store's part (trust, versions, publish with tests, fetch) is in tests/service.rs.
use pgm_workflow_engine_internal::custom::{from_selection, hash, run_tests};
use pgm_workflow_engine_internal::fake::FakeHost;
use pgm_workflow_engine_internal::model::{doc, Document, KindManifest};
use pgm_workflow_engine_internal::{Engine, RunInput, RunOptions, WorkflowHost};
use serde_json::{json, Value};
use std::sync::Arc;

fn d(v: Value) -> Document { doc::from_value(v).unwrap() }
fn manifest(v: Value) -> KindManifest { serde_json::from_value(v).unwrap() }
async fn run(e: &Engine, doc: Document, host: &Arc<FakeHost>, prompt: &str) -> pgm_workflow_engine_internal::RunReport {
    let h: Arc<dyn WorkflowHost> = host.clone();
    e.start(Arc::new(doc), RunInput::prompt(prompt), h, RunOptions::default()).await
}

/// start -> p1 (prompt) -> p2 (prompt, its user from p1) -> end
fn chain() -> Document {
    d(json!({ "schema": 1, "id": "wf-c", "name": "c", "nodes": [{ "id": "s", "kind": "start" }, { "id": "p1", "kind": "prompt" }, { "id": "p2", "kind": "prompt", "config": { "user": "again: {{in}}" } }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "p1.in" }, { "id": "e2", "from": "p1.text", "to": "p2.in" }, { "id": "e3", "from": "p2.text", "to": "e.result" }] }))
}

#[tokio::test]
async fn r431_new_node_from_a_selection_runs_as_its_box() {
    let e0 = Engine::with_builtins();
    let (m, nd) = from_selection(&chain(), e0.registry(), &["p1".into(), "p2".into()], "me/double", "Ask twice").unwrap();
    assert_eq!(m.ports.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["p1_in"], "its ports are the selection's way in ...");
    assert_eq!(m.ports.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["p2_text", "error"], "... and way out");
    assert_eq!(nd.nodes.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>(), ["start", "end", "me/double"]);
    let mut e = Engine::with_builtins();
    e.install(m.clone(), Some(&hash(&m))).unwrap();
    let host = FakeHost::new();
    let r = run(&e, nd, &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!("ai:p2:again: ai:p1:hi")), "the same answer as the two nodes gave before");
    assert_eq!(host.calls_of("p1")[0].key, "p1@k1:sg", "the box's nodes run inside it");
    // the original runs the same
    let r0 = run(&e0, chain(), &FakeHost::new(), "hi").await;
    assert_eq!(r0.result, r.result);
}

#[tokio::test]
async fn r432_undeclared_access_is_denied_and_a_crash_is_the_nodes_error() {
    let mk = |network: Value| manifest(json!({ "schema": 1, "kind": "acme/fetch", "version": "1.0.0", "ports": { "in": [{ "name": "q", "type": "json" }], "out": [{ "name": "result", "type": "json" }, { "name": "error", "type": "error" }] },
        "impl": { "type": "http", "http": { "url": "https://api.example.com/x?q={{inputs.prompt}}" } }, "permissions": { "network": network } }));
    let doc = d(json!({ "schema": 1, "id": "wf-f", "name": "f", "nodes": [{ "id": "s", "kind": "start" }, { "id": "f", "kind": "acme/fetch" }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.inputs", "to": "f.q" }, { "id": "e2", "from": "f.result", "to": "e.result" }] }));
    let mut e = Engine::with_builtins();
    e.install(mk(json!([])), None).unwrap();
    let host = FakeHost::new();
    let r = run(&e, doc.clone(), &host, "x").await;
    assert_eq!(r.error.unwrap()["code"], "engine.denied", "network: [] and it tried a call");
    assert!(host.http_sent().is_empty(), "nothing was sent");
    let mut e = Engine::with_builtins();
    e.install(mk(json!(["api.example.com"])), None).unwrap();
    let host = FakeHost::new();
    host.http_reply(200, r#"{"ok": 1}"#);
    assert_eq!(run(&e, doc, &host, "x").await.result, Some(json!({ "ok": 1 })), "declared: allowed");
    // a script kind that would take too long stops at its step limit; a broken one fails as the node, not the hub
    let slow = manifest(json!({ "schema": 1, "kind": "acme/slow", "version": "1.0.0", "ports": { "in": [{ "name": "in", "type": "json" }], "out": [{ "name": "out", "type": "json" }, { "name": "error", "type": "error" }] },
        "config": { "type": "object" }, "impl": { "type": "script", "code": "out = 1 + 1 + 1 + 1 + 1 + 1", "outputs": ["out"] } }));
    let mut e = Engine::with_builtins();
    e.install(slow, None).unwrap();
    let doc = d(json!({ "schema": 1, "id": "wf-s", "name": "s", "nodes": [{ "id": "s", "kind": "start" }, { "id": "k", "kind": "acme/slow", "config": { "maxSteps": 3 } }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.inputs", "to": "k.in" }, { "id": "e2", "from": "k.out", "to": "e.result" }] }));
    let r = run(&e, doc, &FakeHost::new(), "x").await;
    assert_eq!(r.error.unwrap()["code"], "engine.script-steps");
    let prog = manifest(json!({ "schema": 1, "kind": "acme/prog", "version": "1.0.0", "impl": { "type": "program", "cmd": "rm -rf /" } }));
    assert!(Engine::with_builtins().install(prog, None).unwrap_err()[0].contains("engine.kind-program"), "no sandboxed programs on this runner");
}

#[tokio::test]
async fn r433_publish_runs_the_tests_and_a_failing_one_is_refused() {
    let k = |code: &str| manifest(json!({ "schema": 1, "kind": "acme/inc", "version": "1.0.0", "ports": { "in": [{ "name": "n", "type": "number" }], "out": [{ "name": "out", "type": "number" }, { "name": "error", "type": "error" }] },
        "config": { "type": "object" }, "impl": { "type": "script", "code": code, "outputs": [{ "name": "out", "type": "number" }] },
        "tests": [{ "name": "adds one", "in": { "n": 2 }, "expect": { "out": 3 } }, { "name": "from zero", "in": { "n": 0 }, "expect": { "out": 1 } }] }));
    assert!(run_tests(&k("out = n + 1"), &[]).await.is_empty());
    let f = run_tests(&k("out = n + 2"), &[]).await;
    assert_eq!(f.len(), 2, "{f:?}");
    assert!(f[0].contains("adds one") && f[0].contains("not 3"), "{f:?}");
}

#[tokio::test]
async fn r434_a_major_version_is_never_picked_up_by_an_old_pin_and_a_bad_hash_is_refused() {
    let v = |ver: &str, code: &str| manifest(json!({ "schema": 1, "kind": "acme/tag", "version": ver, "ports": { "in": [{ "name": "in", "type": "json" }], "out": [{ "name": "out", "type": "json" }, { "name": "error", "type": "error" }] },
        "config": { "type": "object" }, "impl": { "type": "script", "code": code, "outputs": [{ "name": "out", "type": "json" }] } }));
    let pinned = |major: u32| d(json!({ "schema": 1, "id": "wf-t", "name": "t", "nodes": [{ "id": "s", "kind": "start" }, { "id": "k", "kind": "acme/tag", "kindVersion": major }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.inputs", "to": "k.in" }, { "id": "e2", "from": "k.out", "to": "e.result" }] }));
    let mut e = Engine::with_builtins();
    e.install(v("2.0.0", "out = 'two'"), None).unwrap();
    let r = run(&e, pinned(1), &FakeHost::new(), "x").await;
    assert_eq!(r.status, "invalid", "a workflow pinned to 1 does not run 2.x");
    assert!(r.problems.iter().any(|p| p["code"] == "kind.version"), "{:?}", r.problems);
    let mut e = Engine::with_builtins();
    e.install(v("1.4.0", "out = 'one'"), None).unwrap();
    assert_eq!(run(&e, pinned(1), &FakeHost::new(), "x").await.result, Some(json!("one")), "a newer minor of the same major runs");
    let good = v("1.4.0", "out = 'one'");
    let h = hash(&good);
    let mut tampered = good.clone();
    tampered.implementation = Some(json!({ "type": "script", "code": "out = 'evil'", "outputs": ["out"] }));
    assert!(Engine::with_builtins().install(tampered, Some(&h)).unwrap_err()[0].contains("engine.kind-hash"));
    assert!(Engine::with_builtins().install(good, Some(&h)).is_ok());
}
