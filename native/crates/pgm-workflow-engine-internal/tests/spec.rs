//! The engine's spec checks (plans/hyper-workflows.md section 16, T-200: R407 to R417), all with the fake host and no
//! network. bridge/test/regress.js runs this file and maps the test names (`r407_...`) back to the checks.
use pgm_workflow_engine_internal::fake::{fake_engine, FakeHost, Reply};
use pgm_workflow_engine_internal::model::{doc, Document};
use pgm_workflow_engine_internal::{Cancel, Engine, RunInput, RunOptions, RunReport, WorkflowHost};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

fn wf(nodes: Value, edges: &[(&str, &str)], extra: Value) -> Arc<Document> {
    let edges: Vec<Value> = edges.iter().enumerate().map(|(i, (f, t))| json!({ "id": format!("e{}", i + 1), "from": f, "to": t })).collect();
    let mut v = json!({ "schema": 1, "id": "wf-test", "name": "test", "nodes": nodes, "edges": edges });
    if let (Some(o), Value::Object(x)) = (v.as_object_mut(), extra) { for (k, val) in x { o.insert(k, val); } }
    Arc::new(doc::from_value(v).expect("a valid document shape"))
}

fn n(id: &str, kind: &str) -> Value { json!({ "id": id, "kind": kind }) }
fn nc(id: &str, kind: &str, config: Value) -> Value { json!({ "id": id, "kind": kind, "config": config }) }
fn ns(id: &str, kind: &str, config: Value, settings: Value) -> Value { json!({ "id": id, "kind": kind, "config": config, "settings": settings }) }
fn h(host: &Arc<FakeHost>) -> Arc<dyn WorkflowHost> { host.clone() }

async fn go(e: &Engine, d: &Arc<Document>, host: &Arc<FakeHost>, prompt: &str) -> RunReport {
    e.start(d.clone(), RunInput::prompt(prompt), h(host), RunOptions::default()).await
}

fn types(r: &RunReport) -> Vec<String> { r.log.iter().map(|e| e["type"].as_str().unwrap().to_string()).collect() }
fn ends_of<'a>(r: &'a RunReport, key: &str) -> Vec<&'a Value> { r.log.iter().filter(|e| e["type"] == "node_end" && e["key"] == key).collect() }
fn status_of(r: &RunReport, key: &str) -> String { ends_of(r, key).last().map(|e| e["status"].as_str().unwrap().to_string()).unwrap_or_default() }
fn started(r: &RunReport, key: &str) -> bool { r.log.iter().any(|e| e["type"] == "node_start" && e["key"] == key) }

/// The log's own guarantees (spec 4.8 / 14): gapless seq, and every node_start has a later node_end unless the run ended
/// cancelled or limit.
fn check_log(r: &RunReport) {
    for (i, e) in r.log.iter().enumerate() { assert_eq!(e["seq"], json!(i as u64 + 1), "gapless seq at {i}: {e}"); }
    if matches!(r.status.as_str(), "cancelled" | "limit" | "waiting") { return; }
    let mut open: Vec<(String, u64)> = vec![];
    for e in &r.log {
        match e["type"].as_str() {
            Some("node_start") => open.push((e["key"].as_str().unwrap().to_string(), e["attempt"].as_u64().unwrap_or(1))),
            Some("node_end") => { if let Some(p) = open.iter().position(|(k, _)| e["key"] == json!(k)) { open.remove(p); } }
            _ => {}
        }
    }
    assert!(open.is_empty(), "node_start without node_end: {open:?}");
}

// ---------------------------------------------------------------- R407 sequential

fn simple() -> Arc<Document> {
    wf(json!([n("s", "start"), n("p", "prompt"), n("e", "end")]), &[("s.prompt", "p.in"), ("p.text", "e.result")], json!({}))
}

#[tokio::test]
async fn r407_prompt_through_start_prompt_end() {
    let e = fake_engine();
    let host = FakeHost::new();
    let r = go(&e, &simple(), &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.outcome.as_deref(), Some("done"));
    assert_eq!(r.result, Some(json!("ai:p:hi")));
    assert_eq!(types(&r), ["run_start", "node_start", "node_end", "node_start", "node_end", "node_start", "node_end", "run_end"]);
    check_log(&r);
    assert_eq!(host.log(), r.log, "every event went to the host's log sink, in order");
    let rs = &r.log[0];
    assert_eq!(rs["prompt"], "hi");
    assert_eq!(rs["workflow"]["name"], "test");
    assert_eq!(r.log.last().unwrap()["totals"]["aiCalls"], 1);
}

#[tokio::test]
async fn r407_templates_read_inputs_and_earlier_nodes() {
    let e = fake_engine();
    let host = FakeHost::new();
    let d = wf(json!([n("s", "start"), n("p1", "prompt"), nc("p2", "prompt", json!({ "user": "Q: {{inputs.prompt}} / {{node.p1.out.text}} / {{inputs.audience}}" })), n("e", "end")]),
        &[("s.prompt", "p1.in"), ("p1.text", "p2.in"), ("p2.text", "e.result")],
        json!({ "inputs": [{ "name": "audience", "type": "text", "default": "team" }] }));
    let r = go(&e, &d, &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(host.calls_of("p2")[0].prompt, "Q: hi / ai:p1:hi / team");
}

// ---------------------------------------------------------------- R408 parallel lines

fn lines(join: Value, limits: Value) -> Arc<Document> {
    wf(json!([n("s", "start"), nc("par", "parallel", json!({ "mode": "copies", "n": 3 })), n("p", "prompt"), nc("j", "join", join), n("e", "end")]),
        &[("s.prompt", "par.in"), ("par.out", "p.in"), ("p.text", "j.lines"), ("j.result", "e.result")], json!({ "defaults": { "limits": limits } }))
}

fn staggered(host: &Arc<FakeHost>) {
    host.reply("p@par:l1", Reply::after(120, Reply::text("one")));
    host.reply("p@par:l2", Reply::after(60, Reply::text("two")));
    host.reply("p@par:l3", Reply::after(10, Reply::text("three")));
}

#[tokio::test]
async fn r408_lines_overlap_and_join_all_keeps_line_order() {
    let e = fake_engine();
    let host = FakeHost::new();
    staggered(&host);
    let r = go(&e, &lines(json!({ "mode": "all" }), json!({})), &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!(["one", "two", "three"])), "line order, not finishing order");
    assert_eq!(host.most_at_once(), 3, "the three lines ran at the same time");
    let line_of = |k: &str| r.log.iter().find(|e| e["type"] == "node_start" && e["key"] == k).unwrap()["line"].clone();
    assert_eq!(line_of("p@par:l2"), "l2");
    check_log(&r);
}

#[tokio::test]
async fn r408_join_first_cancels_the_other_lines() {
    let e = fake_engine();
    let host = FakeHost::new();
    staggered(&host);
    let r = go(&e, &lines(json!({ "mode": "first" }), json!({})), &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!("three")), "the fastest line wins");
    let calls = host.all_calls();
    assert!(calls.iter().filter(|c| c.key != "p@par:l3").all(|c| c.cancelled), "the slower lines were cancelled: {calls:?}");
    assert_eq!(status_of(&r, "p@par:l1"), "cancelled");
    check_log(&r);
}

#[tokio::test]
async fn r408_max_parallel_two_never_runs_three() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.delay("p", 40);
    let r = go(&e, &lines(json!({ "mode": "all" }), json!({ "maxParallel": 2 })), &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(host.most_at_once(), 2);
    assert_eq!(r.result.unwrap().as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn r408_split_one_line_per_item() {
    let e = fake_engine();
    let host = FakeHost::new();
    let d = wf(json!([n("s", "start"), nc("c", "test/const", json!({ "value": ["a", "b"] })), nc("par", "parallel", json!({ "mode": "split" })), n("p", "prompt"), nc("j", "join", json!({})), n("e", "end")]),
        &[("s.prompt", "c.in"), ("c.out", "par.in"), ("par.out", "p.in"), ("p.text", "j.lines"), ("j.result", "e.result")], json!({}));
    let r = go(&e, &d, &host, "hi").await;
    assert_eq!(r.result, Some(json!(["ai:p:a", "ai:p:b"])), "{r:?}");
}

#[tokio::test]
async fn r408_fixed_lines_and_a_plain_join_first() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.delay("p1", 200).delay("p2", 10);
    let d = wf(json!([n("s", "start"), nc("par", "parallel", json!({ "mode": "fixed", "lines": ["x", "y"] })), n("p1", "prompt"), n("p2", "prompt"), n("p3", "prompt"), nc("j", "join", json!({ "mode": "first" })), n("e", "end")]),
        &[("s.prompt", "par.in"), ("par.x", "p1.in"), ("p1.text", "p3.in"), ("par.y", "p2.in"), ("p3.text", "j.lines"), ("p2.text", "j.lines"), ("j.result", "e.result")], json!({}));
    let r = go(&e, &d, &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!("ai:p2:hi")));
    assert!(host.calls_of("p1")[0].cancelled, "the slow line was cancelled");
    assert!(host.calls_of("p3").is_empty(), "and the node after it never ran");
    assert_eq!(status_of(&r, "p3"), "skipped");
    check_log(&r);
}

#[tokio::test]
async fn r408_a_failed_line_and_the_joins_on_fail() {
    let e = fake_engine();
    for (on_fail, want) in [("skip", 2usize), ("collect", 3)] {
        let host = FakeHost::new();
        host.reply("p@par:l2", Reply::fail("boom"));
        let r = go(&e, &lines(json!({ "mode": "all", "onFail": on_fail }), json!({})), &host, "hi").await;
        assert_eq!(r.status, "ok", "{on_fail}: {r:?}");
        let list = r.result.unwrap();
        assert_eq!(list.as_array().unwrap().len(), want, "{on_fail}: {list}");
        if on_fail == "collect" { assert_eq!(list[1]["code"], "boom"); }
    }
    let host = FakeHost::new();
    host.reply("p@par:l2", Reply::fail("boom"));
    let r = go(&e, &lines(json!({ "mode": "all" }), json!({})), &host, "hi").await;
    assert_eq!(r.status, "failed", "onFail: fail (the default) fails the run when nothing catches it");
    assert_eq!(r.error.unwrap()["code"], "boom");
}

// ---------------------------------------------------------------- R409 switch

#[tokio::test]
async fn r409_switch_fires_one_port_and_the_rest_is_skipped() {
    let e = fake_engine();
    let host = FakeHost::new();
    let d = wf(json!([n("s", "start"), nc("sw", "switch", json!({ "cases": ["a", { "port": "b", "equals": "b" }] })), n("pa", "prompt"), n("pb", "prompt"), n("pd", "prompt"), nc("j", "join", json!({})), n("e", "end")]),
        &[("s.prompt", "sw.value"), ("sw.a", "pa.in"), ("sw.b", "pb.in"), ("sw.default", "pd.in"), ("pa.text", "j.lines"), ("pb.text", "j.lines"), ("pd.text", "j.lines"), ("j.result", "e.result")], json!({}));
    let r = go(&e, &d, &host, "b").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!(["ai:pb:b"])), "the join leaves the skipped lines out");
    assert_eq!(host.all_calls().len(), 1);
    assert_eq!(status_of(&r, "pa"), "skipped");
    assert_eq!(status_of(&r, "pd"), "skipped");
    assert_eq!(ends_of(&r, "sw")[0]["port"], "b");
    let r = go(&e, &d, &FakeHost::new(), "zzz").await;
    assert_eq!(r.result, Some(json!(["ai:pd:zzz"])), "nothing fits: default");
    check_log(&r);
}

#[tokio::test]
async fn r409_if_takes_one_way() {
    let e = fake_engine();
    let d = wf(json!([n("s", "start"), nc("i", "if", json!({ "condition": "len(value) > 3" })), n("pt", "prompt"), n("pf", "prompt"), nc("e1", "end", json!({ "name": "long" })), nc("e2", "end", json!({ "name": "short" }))]),
        &[("s.prompt", "i.value"), ("i.true", "pt.in"), ("i.false", "pf.in"), ("pt.text", "e1.result"), ("pf.text", "e2.result")], json!({}));
    assert_eq!(go(&e, &d, &FakeHost::new(), "hello").await.outcome.as_deref(), Some("long"));
    assert_eq!(go(&e, &d, &FakeHost::new(), "hi").await.outcome.as_deref(), Some("short"));
}

// ---------------------------------------------------------------- R410 loops

fn counter(cond: &str, max: u64, limit_wired: bool) -> Arc<Document> {
    let mut nodes = vec![n("s", "start"), nc("c", "test/const", json!({ "value": 0 })), nc("l", "loop", json!({ "mode": "until", "condition": cond, "maxIterations": max })), n("inc", "test/inc"), n("e", "end")];
    let mut edges = vec![("s.prompt", "c.in"), ("c.out", "l.in"), ("l.again", "inc.in"), ("inc.out", "l.back"), ("l.done", "e.result")];
    if limit_wired { nodes.push(nc("e2", "end", json!({ "name": "too-many" }))); edges.push(("l.limit", "e2.result")); }
    wf(Value::Array(nodes), &edges, json!({}))
}

#[tokio::test]
async fn r410_until_stops_when_met_and_each_round_is_logged() {
    let e = fake_engine();
    let r = go(&e, &counter("value >= 3", 10, false), &FakeHost::new(), "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!(3)));
    for k in ["inc@l:1", "inc@l:2", "inc@l:3"] { assert_eq!(status_of(&r, k), "ok", "{k}"); }
    assert!(!started(&r, "inc@l:4"));
    let it = r.log.iter().find(|e| e["type"] == "node_start" && e["key"] == "inc@l:2").unwrap();
    assert_eq!(it["iteration"], 2);
    assert_eq!(ends_of(&r, "l")[0]["outputs"]["round"], 3);
    check_log(&r);
}

#[tokio::test]
async fn r410_a_loop_that_never_ends_stops_at_its_limit() {
    let e = fake_engine();
    let r = go(&e, &counter("value >= 100", 4, true), &FakeHost::new(), "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.outcome.as_deref(), Some("too-many"), "it left by its `limit` port");
    assert_eq!(r.result, Some(json!(4)));
    assert_eq!(r.events("limit")[0]["limit"], "maxIterations");
    let r = go(&e, &counter("value >= 100", 4, false), &FakeHost::new(), "hi").await;
    assert_eq!(r.status, "gave-up", "with no `limit` wire the run gives up: {r:?}");
    assert_eq!(r.error.unwrap()["code"], "engine.gave-up");
}

#[tokio::test]
async fn r410_while_and_count() {
    let e = fake_engine();
    let d = |cfg: Value| wf(json!([n("s", "start"), nc("c", "test/const", json!({ "value": 0 })), nc("l", "loop", cfg), nc("inc", "test/inc", json!({ "by": 2 })), n("e", "end")]),
        &[("s.prompt", "c.in"), ("c.out", "l.in"), ("l.again", "inc.in"), ("inc.out", "l.back"), ("l.done", "e.result")], json!({}));
    assert_eq!(go(&e, &d(json!({ "mode": "count", "count": 5, "maxIterations": 5 })), &FakeHost::new(), "x").await.result, Some(json!(10)));
    assert_eq!(go(&e, &d(json!({ "mode": "while", "condition": "value < 7", "maxIterations": 10 })), &FakeHost::new(), "x").await.result, Some(json!(8)));
    assert_eq!(go(&e, &d(json!({ "mode": "while", "condition": "value > 7", "maxIterations": 10 })), &FakeHost::new(), "x").await.result, Some(json!(0)), "a while that is false at once runs no round");
}

#[tokio::test]
async fn r410_a_loop_inside_each_parallel_line() {
    let e = fake_engine();
    let d = wf(json!([n("s", "start"), nc("c", "test/const", json!({ "value": [1, 10] })), nc("par", "parallel", json!({ "mode": "split" })),
            nc("l", "loop", json!({ "mode": "count", "count": 3, "maxIterations": 3 })), n("inc", "test/inc"), nc("j", "join", json!({})), n("e", "end")]),
        &[("s.prompt", "c.in"), ("c.out", "par.in"), ("par.out", "l.in"), ("l.again", "inc.in"), ("inc.out", "l.back"), ("l.done", "j.lines"), ("j.result", "e.result")], json!({}));
    let r = go(&e, &d, &FakeHost::new(), "x").await;
    assert_eq!(r.result, Some(json!([4, 13])), "{r:?}");
    assert_eq!(status_of(&r, "inc@par:l2/l:3"), "ok");
    check_log(&r);
}

// ---------------------------------------------------------------- R411 retries and failure paths

fn retrying(settings: Value, error_wire: bool) -> Arc<Document> {
    let mut nodes = vec![n("s", "start"), ns("p", "prompt", json!({}), settings), n("e", "end")];
    let mut edges = vec![("s.prompt", "p.in"), ("p.text", "e.result")];
    if error_wire { nodes.push(n("fix", "prompt")); nodes.push(nc("e2", "end", json!({ "name": "fixed" }))); edges.push(("p.error", "fix.in")); edges.push(("fix.text", "e2.result")); }
    wf(Value::Array(nodes), &edges, json!({}))
}

#[tokio::test]
async fn r411_fails_twice_then_works_in_three_attempts_with_backoff() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.replies("p", vec![Reply::fail("flaky"), Reply::fail("flaky"), Reply::text("finally")]);
    let r = go(&e, &retrying(json!({ "retry": { "max": 2, "backoffSec": 0.01 } }), false), &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!("finally")));
    assert_eq!(host.calls_of("p").iter().map(|c| c.attempt).collect::<Vec<_>>(), [1, 2, 3]);
    let retries = r.events("node_retry");
    assert_eq!(retries.len(), 2);
    assert_eq!((retries[0]["afterMs"].as_u64(), retries[1]["afterMs"].as_u64()), (Some(10), Some(20)), "the backoff grows");
    assert_eq!(retries[0]["reason"], "error");
    check_log(&r);
}

#[tokio::test]
async fn r411_after_max_it_leaves_by_its_error_port() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.replies("p", vec![Reply::fail("x"), Reply::fail("x"), Reply::fail("x")]);
    let r = go(&e, &retrying(json!({ "retry": { "max": 2, "backoffSec": 0 } }), true), &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.outcome.as_deref(), Some("fixed"));
    assert!(host.calls_of("fix")[0].prompt.contains("\"code\":\"x\""), "the failure is a value on the error wire");
    assert_eq!(ends_of(&r, "p").last().unwrap()["port"], "error");
}

#[tokio::test]
async fn r411_no_wire_on_error_fail_or_skip() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.reply("p", Reply::fail("x"));
    let r = go(&e, &retrying(json!({}), false), &host, "hi").await;
    assert_eq!(r.status, "failed", "{r:?}");
    assert_eq!(r.error.as_ref().unwrap()["code"], "x");
    assert_eq!(r.error.as_ref().unwrap()["node"], "p");
    assert_eq!(status_of(&r, "e"), "error", "the failure reached the end, which failed the run");
    check_log(&r);
    let host = FakeHost::new();
    host.reply("p", Reply::fail("x"));
    let r = go(&e, &retrying(json!({ "onError": "skip" }), false), &host, "hi").await;
    assert_eq!(r.status, "ok", "onError: skip goes on: {r:?}");
    assert_eq!(r.result, Some(Value::Null));
    assert!(ends_of(&r, "p")[0]["warning"].as_str().unwrap().contains("skip"));
}

#[tokio::test]
async fn r411_a_timeout_is_a_retry_reason() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.replies("p", vec![Reply::after(500, Reply::text("late")), Reply::text("quick")]);
    let r = go(&e, &retrying(json!({ "timeoutSec": 0.05, "retry": { "max": 1, "backoffSec": 0, "on": ["timeout"] } }), false), &host, "hi").await;
    assert_eq!(r.result, Some(json!("quick")), "{r:?}");
    assert_eq!(r.events("node_retry")[0]["reason"], "timeout");
}

// ---------------------------------------------------------------- R412 approvals

fn approval(on_timeout: Option<&str>) -> Arc<Document> {
    let mut cfg = json!({ "timeoutSec": 60 });
    if let Some(t) = on_timeout { cfg["onTimeout"] = json!(t); }
    wf(json!([n("s", "start"), nc("a", "approve", cfg), n("e", "end"), nc("e2", "end", json!({ "name": "rejected" }))]),
        &[("s.prompt", "a.in"), ("a.approved", "e.result"), ("a.rejected", "e2.result")], json!({}))
}

#[tokio::test]
async fn r412_approval_waits_with_no_worker_and_an_answer_wakes_it() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.set_clock(1_000_000);
    let d = approval(Some("reject"));
    let r = go(&e, &d, &host, "ship it").await;
    assert_eq!(r.status, "waiting", "{r:?}");
    assert_eq!(r.waiting[0]["key"], "a");
    assert_eq!(r.waiting[0]["deadline"], 1_060_000);
    assert_eq!(host.asks.lock().unwrap().len(), 1);
    assert!(r.events("snapshot").len() == 1 && !host.snapshots.lock().unwrap().is_empty(), "a snapshot is written");
    assert!(r.events("run_end").is_empty(), "the run has not ended");
    // a resume before the deadline does not ask again and still waits
    let r2 = e.resume(d.clone(), r.log.clone(), h(&host), RunOptions::default()).await;
    assert_eq!(r2.status, "waiting");
    assert_eq!(host.asks.lock().unwrap().len(), 1, "asked once");
    let r3 = e.answer(d.clone(), r2.log.clone(), "a", json!({ "approved": true }), "user", h(&host), RunOptions::default()).await;
    assert_eq!(r3.status, "ok", "{r3:?}");
    assert_eq!(r3.outcome.as_deref(), Some("done"));
    assert_eq!(r3.result, Some(json!("ship it")));
    assert_eq!(r3.events("answer")[0]["by"], "user");
    check_log(&r3);
    let again = e.answer(d.clone(), r3.log.clone(), "a", json!({ "approved": true }), "user", h(&host), RunOptions::default()).await;
    assert_eq!(again.status, "invalid", "a question is answered once");
}

#[tokio::test]
async fn r412_a_timeout_follows_on_timeout_and_never_approves_unasked() {
    let e = fake_engine();
    for (on_timeout, want) in [(None, "rejected"), (Some("reject"), "rejected"), (Some("approve"), "done")] {
        let host = FakeHost::new();
        host.set_clock(5_000);
        let d = approval(on_timeout);
        let r = go(&e, &d, &host, "x").await;
        assert_eq!(r.status, "waiting");
        host.advance(60_001);
        let r = e.resume(d, r.log, h(&host), RunOptions::default()).await;
        assert_eq!(r.outcome.as_deref(), Some(want), "onTimeout {on_timeout:?}: {r:?}");
        assert_eq!(r.events("answer")[0]["by"], "timeout");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r412_an_approval_in_every_round_of_a_loop() {
    let e = fake_engine();
    let host = FakeHost::new();
    let d = wf(json!([n("s", "start"), nc("c", "test/const", json!({ "value": 0 })), nc("l", "loop", json!({ "mode": "count", "count": 2, "maxIterations": 2 })),
            n("inc", "test/inc"), nc("a", "approve", json!({ "timeoutSec": 600 })), n("e", "end")]),
        &[("s.prompt", "c.in"), ("c.out", "l.in"), ("l.again", "inc.in"), ("inc.out", "a.in"), ("a.approved", "l.back"), ("l.done", "e.result")], json!({}));
    let r = go(&e, &d, &host, "x").await;
    assert_eq!((r.status.as_str(), r.waiting[0]["key"].as_str()), ("waiting", Some("a@l:1")), "{r:?}");
    let r = e.answer(d.clone(), r.log, "a@l:1", json!(["yes"]), "user", h(&host), RunOptions::default()).await;
    assert_eq!((r.status.as_str(), r.waiting[0]["key"].as_str()), ("waiting", Some("a@l:2")), "round 2 asks again: {r:?}");
    let r = e.answer(d.clone(), r.log, "a@l:2", json!(["yes"]), "user", h(&host), RunOptions::default()).await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!(2)));
    assert_eq!(host.asks.lock().unwrap().len(), 2);
    assert_eq!(r.events("node_start").iter().filter(|e| e["key"] == "inc@l:1").count(), 1, "round 1 ran once over three drives");
    check_log(&r);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r408_lines_on_a_multi_thread_runtime() {
    let e = fake_engine();
    let host = FakeHost::new();
    staggered(&host);
    let r = go(&e, &lines(json!({ "mode": "all" }), json!({})), &host, "hi").await;
    assert_eq!(r.result, Some(json!(["one", "two", "three"])), "{r:?}");
    let host = FakeHost::new();
    staggered(&host);
    let r = go(&e, &lines(json!({ "mode": "first" }), json!({})), &host, "hi").await;
    assert_eq!(r.result, Some(json!("three")), "{r:?}");
    check_log(&r);
}

// ---------------------------------------------------------------- R413 resume

fn chain() -> Arc<Document> {
    wf(json!([n("s", "start"), n("p1", "prompt"), n("p2", "prompt"), n("p3", "prompt"), n("e", "end")]),
        &[("s.prompt", "p1.in"), ("p1.text", "p2.in"), ("p2.text", "p3.in"), ("p3.text", "e.result")], json!({}))
}

#[tokio::test]
async fn r413_a_killed_run_resumes_without_running_finished_nodes_again() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.reply("p3", Reply::Hang);
    let d = chain();
    let killed = tokio::time::timeout(Duration::from_millis(150), go(&e, &d, &host, "hi")).await;
    assert!(killed.is_err(), "the run was cut while p3 ran");
    let log = host.log();
    assert!(log.iter().any(|e| e["type"] == "node_start" && e["key"] == "p3"));
    let fresh = FakeHost::new();
    let r = e.resume(d, log, h(&fresh), RunOptions::default()).await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!("ai:p3:ai:p2:ai:p1:hi")), "p1 and p2's outputs came from the log");
    assert!(fresh.calls_of("p1").is_empty() && fresh.calls_of("p2").is_empty(), "nodes 1 and 2 did not run again");
    assert_eq!(fresh.calls_of("p3")[0].attempt, 2, "the cut node runs again, counted as another attempt");
    assert_eq!(r.events("run_resume").len(), 1);
    assert_eq!(r.run, r.log[0]["run"].as_str().unwrap());
}

#[tokio::test]
async fn r413_a_cut_node_that_is_not_idempotent_waits_for_a_person() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.reply("x", Reply::Hang);
    let d = wf(json!([n("s", "start"), n("x", "test/side-effect"), n("e", "end")]), &[("s.inputs", "x.args"), ("x.result", "e.result")], json!({}));
    assert!(tokio::time::timeout(Duration::from_millis(100), go(&e, &d, &host, "hi")).await.is_err());
    let fresh = FakeHost::new();
    let r = e.resume(d.clone(), host.log(), h(&fresh), RunOptions::default()).await;
    assert_eq!(r.status, "waiting", "{r:?}");
    assert_eq!(r.waiting[0]["what"], "resume");
    assert_eq!(fresh.asks.lock().unwrap()[0].what, "resume");
    assert!(fresh.tools.lock().unwrap().is_empty(), "it did not run again on its own");
    let r = e.answer(d, r.log, "x", json!({ "rerun": true }), "user", h(&fresh), RunOptions::default()).await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(fresh.tools.lock().unwrap().len(), 1);
}

// ---------------------------------------------------------------- R414 limits

#[tokio::test]
async fn r414_max_nodes_max_seconds_and_max_cost() {
    let e = fake_engine();
    let d = wf(json!([n("s", "start"), nc("c", "test/const", json!({ "value": 0 })), nc("l", "loop", json!({ "mode": "count", "count": 50, "maxIterations": 50 })), n("inc", "test/inc"), n("e", "end")]),
        &[("s.prompt", "c.in"), ("c.out", "l.in"), ("l.again", "inc.in"), ("inc.out", "l.back"), ("l.done", "e.result")], json!({ "defaults": { "limits": { "maxNodes": 10 } } }));
    let r = go(&e, &d, &FakeHost::new(), "x").await;
    assert_eq!((r.status.as_str(), r.events("limit")[0]["limit"].as_str()), ("limit", Some("maxNodes")), "{r:?}");
    assert_eq!(r.log.last().unwrap()["type"], "run_end");

    let host = FakeHost::new();
    host.delay("p1", 2000);
    let d = wf(json!([n("s", "start"), n("p1", "prompt"), n("e", "end")]), &[("s.prompt", "p1.in"), ("p1.text", "e.result")], json!({ "defaults": { "limits": { "maxSeconds": 0.1 } } }));
    let t = std::time::Instant::now();
    let r = go(&e, &d, &host, "x").await;
    assert_eq!((r.status.as_str(), r.error.as_ref().unwrap()["limit"].as_str()), ("limit", Some("maxSeconds")), "{r:?}");
    assert!(t.elapsed() < Duration::from_millis(1500) && host.calls_of("p1")[0].cancelled, "the running node was stopped");

    let host = FakeHost::new();
    host.reply("p1", Reply::cost(1.5, Reply::text("a"))).reply("p2", Reply::cost(1.0, Reply::text("b")));
    let d = wf(json!([n("s", "start"), n("p1", "prompt"), n("p2", "prompt"), n("p3", "prompt"), n("e", "end")]),
        &[("s.prompt", "p1.in"), ("p1.text", "p2.in"), ("p2.text", "p3.in"), ("p3.text", "e.result")], json!({ "defaults": { "limits": { "maxCost": 2.0 } } }));
    let r = go(&e, &d, &host, "x").await;
    assert_eq!((r.status.as_str(), r.error.as_ref().unwrap()["limit"].as_str()), ("limit", Some("maxCost")), "{r:?}");
    assert!(host.calls_of("p3").is_empty());
    // the policy's ceiling is lower than the document's
    let opts = RunOptions { limits: pgm_workflow_engine_internal::RunLimits { max_nodes: Some(2), ..Default::default() }, ..Default::default() };
    let r = e.start(simple(), RunInput::prompt("x"), h(&FakeHost::new()), opts).await;
    assert_eq!(r.status, "limit");
}

// ---------------------------------------------------------------- R415 cancel

#[tokio::test]
async fn r415_cancel_stops_running_nodes_and_starts_none() {
    let e = fake_engine();
    let host = FakeHost::new();
    host.delay("p1", 3000);
    let d = chain();
    let cancel = Cancel::new();
    let opts = RunOptions { cancel: cancel.clone(), ..Default::default() };
    let (r, _) = tokio::join!(e.start(d, RunInput::prompt("x"), h(&host), opts), async { tokio::time::sleep(Duration::from_millis(50)).await; cancel.cancel(); });
    assert_eq!(r.status, "cancelled", "{r:?}");
    assert!(host.calls_of("p1")[0].cancelled, "the AI call saw its cancel");
    assert_eq!(status_of(&r, "p1"), "cancelled");
    assert!(!started(&r, "p2"), "no node started after the cancel");
    assert_eq!(r.log.last().unwrap()["status"], "cancelled");
}

// ---------------------------------------------------------------- R416 panics

#[tokio::test]
async fn r416_a_panicking_node_fails_alone() {
    let e = fake_engine();
    let bad = FakeHost::new();
    bad.reply("p", Reply::Panic);
    let good = FakeHost::new();
    good.delay("p", 30);
    let d = simple();
    let (r1, r2) = tokio::join!(go(&e, &d, &bad, "a"), go(&e, &d, &good, "b"));
    assert_eq!(r1.status, "failed", "{r1:?}");
    assert_eq!(r1.error.unwrap()["code"], "engine.panic");
    assert_eq!(r2.status, "ok", "the other run went on: {r2:?}");
    // and with an error wire, a panic is just a failure to catch
    let bad = FakeHost::new();
    bad.reply("p", Reply::Panic);
    let r = go(&e, &retrying(json!({}), true), &bad, "a").await;
    assert_eq!(r.outcome.as_deref(), Some("fixed"), "{r:?}");
}

// ---------------------------------------------------------------- R417 replay

#[tokio::test]
async fn r417_replay_gives_the_same_result_with_no_calls() {
    let e = fake_engine();
    let d = wf(json!([n("s", "start"), nc("sw", "switch", json!({ "cases": ["a"] })), n("pa", "prompt"), n("pd", "prompt"), nc("j", "join", json!({})), n("e", "end")]),
        &[("s.prompt", "sw.value"), ("sw.a", "pa.in"), ("sw.default", "pd.in"), ("pa.text", "j.lines"), ("pd.text", "j.lines"), ("j.result", "e.result")], json!({}));
    let first = go(&e, &d, &FakeHost::new(), "a").await;
    assert_eq!(first.status, "ok");
    let refuse = FakeHost::new();
    refuse.refuse_all();
    let r = e.replay(d, first.log.clone(), h(&refuse), RunOptions::default()).await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!((r.result.clone(), r.outcome.clone()), (first.result.clone(), first.outcome.clone()));
    assert_ne!(r.run, first.run, "a replay is a new run");
    assert_eq!(r.log[0]["replayOf"], json!(first.run));
    assert!(refuse.all_calls().is_empty());
    assert!(ends_of(&r, "pa")[0]["replayed"] == true);
    check_log(&r);
}

// ---------------------------------------------------------------- sub-graphs, for-each, preflight, small kinds

fn boxed(top_nodes: Value, top_edges: &[(&str, &str)]) -> Arc<Document> {
    wf(top_nodes, top_edges, json!({ "subgraphs": { "box": {
        "inputs": [{ "name": "x", "type": "json" }], "outputs": [{ "name": "y", "type": "text" }],
        "nodes": [nc("i", "input", json!({ "name": "x", "type": "json" })), n("p", "prompt"), nc("o", "output", json!({ "name": "y", "type": "text" }))],
        "edges": [{ "id": "b1", "from": "i.value", "to": "p.in" }, { "id": "b2", "from": "p.text", "to": "o.value" }],
    } } }))
}

#[tokio::test]
async fn subgraph_box_with_inputs_and_outputs() {
    let e = fake_engine();
    let host = FakeHost::new();
    let d = boxed(json!([n("s", "start"), nc("b", "subgraph", json!({ "ref": "box" })), n("e", "end")]), &[("s.prompt", "b.x"), ("b.y", "e.result")]);
    let r = go(&e, &d, &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!("ai:p:hi")));
    assert_eq!(host.calls_of("p")[0].key, "p@b:sg");
    check_log(&r);
}

#[tokio::test]
async fn for_each_runs_the_box_once_per_item() {
    let e = fake_engine();
    let d = boxed(json!([n("s", "start"), nc("f", "for-each", json!({ "ref": "box" })), n("e", "end")]), &[("s.files", "f.items"), ("f.results", "e.result")]);
    let input = RunInput { prompt: "x".into(), files: vec![json!("a"), json!("b"), json!("c")], ..Default::default() };
    let r = e.start(d, input, h(&FakeHost::new()), RunOptions::default()).await;
    assert_eq!(r.result, Some(json!(["ai:p:a", "ai:p:b", "ai:p:c"])), "{r:?}");
}

#[tokio::test]
async fn preflight_refuses_before_anything_runs() {
    let host = FakeHost::new();
    let r = Engine::new().start(simple(), RunInput::prompt("x"), h(&host), RunOptions::default()).await;
    assert_eq!(r.status, "invalid");
    assert!(r.problems.iter().any(|p| p["code"] == "engine.kind-missing" && p["node"] == "p"), "{:?}", r.problems);
    assert!(r.log.is_empty() && host.log().is_empty(), "nothing was logged, nothing ran");
    let two = wf(json!([n("s", "start"), n("s2", "start"), n("e", "end")]), &[("s.prompt", "e.result")], json!({}));
    let r = fake_engine().start(two, RunInput::prompt("x"), h(&host), RunOptions::default()).await;
    assert!(r.problems.iter().any(|p| p["code"] == "start.multiple"), "{:?}", r.problems);
    let nojoin = wf(json!([n("s", "start"), nc("par", "parallel", json!({ "mode": "copies", "n": 2 })), n("p", "prompt"), n("e", "end")]), &[("s.prompt", "par.in"), ("par.out", "p.in"), ("p.text", "e.result")], json!({}));
    assert!(fake_engine().preflight(&nojoin).iter().any(|p| p["code"] == "engine.parallel-join"));
}

#[tokio::test]
async fn set_and_emit() {
    let e = fake_engine();
    let host = FakeHost::new();
    let d = wf(json!([n("s", "start"), nc("st", "set", json!({ "key": "topic" })), n("em", "emit"), nc("p", "prompt", json!({ "user": "about {{state.topic}}" })), n("e", "end")]),
        &[("s.prompt", "st.value"), ("st.out", "p.in"), ("s.prompt", "em.message"), ("p.text", "e.result")], json!({}));
    let r = go(&e, &d, &host, "cats").await;
    assert_eq!(r.result, Some(json!("ai:p:about cats")), "{r:?}");
    assert_eq!(*host.emits.lock().unwrap(), vec!["cats".to_string()]);
}
