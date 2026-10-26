//! The `rea` node (T-255): a workflow step over the host's standalone RE tools. A stand-in host plays the RE tools
//! (copies, a queue, REA's MCP answers), so the node's behaviour is checked with no container engine.
use pgm_workflow_engine_internal::model::{doc, Document};
use pgm_workflow_engine_internal::{BoxFut, Cancel, Engine, NodeError, RunInput, RunOptions, RunReport, ToolCtx, WorkflowHost};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The RE tools as a host has them: copies with a state, files, one REA tool (`echo`) and one that fails (`broken`).
#[derive(Default)]
struct Re {
    calls: Mutex<Vec<(String, Value)>>,
    copies: Mutex<HashMap<String, (String, u32)>>, // id -> (state, status polls left while queued)
    files: Mutex<HashMap<String, String>>,
    queued_polls: u32,
    refuse: bool,
    n: Mutex<u32>,
}
impl Re {
    fn queued(polls: u32) -> Re { Re { queued_polls: polls, ..Re::default() } }
    fn names(&self) -> Vec<String> { self.calls.lock().unwrap().iter().map(|c| c.0.clone()).collect() }
    fn running(&self) -> usize { self.copies.lock().unwrap().values().filter(|c| c.0 != "gone").count() }
}
impl WorkflowHost for Re {
    fn call_tool<'a>(&'a self, name: String, args: Value, _ctx: ToolCtx, _cancel: Cancel) -> BoxFut<'a, Result<Value, NodeError>> {
        Box::pin(async move {
            if self.refuse { return Err(NodeError::fatal("engine.tool-denied", format!("the tool `{name}` is not allowed in this workflow's scope"))); }
            self.calls.lock().unwrap().push((name.clone(), args.clone()));
            let id = args["id"].as_str().unwrap_or("").to_string();
            let live = |id: &str| self.copies.lock().unwrap().get(id).is_some_and(|c| c.0 != "gone");
            match name.as_str() {
                "re_start" => {
                    let mut n = self.n.lock().unwrap(); *n += 1;
                    let id = format!("re-{n}");
                    let state = if self.queued_polls > 0 { "queued" } else { "running" };
                    self.copies.lock().unwrap().insert(id.clone(), (state.into(), self.queued_polls));
                    Ok(json!({ "sandbox": { "id": id, "state": state, "purpose": args["purpose"] } }))
                }
                "re_status" => {
                    let mut c = self.copies.lock().unwrap();
                    let e = c.get_mut(&id).ok_or_else(|| NodeError::new("engine.tool", "No such RE container."))?;
                    if e.0 == "queued" { e.1 = e.1.saturating_sub(1); if e.1 == 0 { e.0 = "running".into(); } }
                    Ok(json!({ "sandbox": { "id": id, "state": e.0 } }))
                }
                "re_put" if live(&id) => { self.files.lock().unwrap().insert(args["path"].as_str().unwrap_or("").into(), args["text"].as_str().unwrap_or("").into()); Ok(json!({ "path": args["path"] })) }
                "re_exec" if live(&id) => {
                    let cmd = args["cmd"].as_str().unwrap_or("").to_string();
                    Ok(if cmd.starts_with("false") { json!({ "exit": 1, "stdout": "", "stderr": "it failed" }) } else { json!({ "exit": 0, "stdout": format!("ran {cmd}"), "stderr": "" }) })
                }
                "re_call" if live(&id) => match args["tool"].as_str() {
                    Some("echo") => Ok(json!({ "content": [{ "type": "text", "text": format!("echo {}", args["args"]) }] })),
                    Some("broken") => Ok(json!({ "isError": true, "content": [{ "type": "text", "text": "no such binary" }] })),
                    _ => Err(NodeError::new("engine.tool", "REA has no such tool.")),
                },
                "re_get" if live(&id) => Ok(json!({ "path": args["path"], "text": self.files.lock().unwrap().get(args["path"].as_str().unwrap_or("")).cloned().unwrap_or_default() })),
                "re_stop" => { if let Some(c) = self.copies.lock().unwrap().get_mut(&id) { c.0 = "gone".into(); } Ok(json!({ "deleted": true })) }
                _ => Err(NodeError::new("engine.tool", format!("{name}: no running copy {id}"))),
            }
        })
    }
}

fn wf(nodes: Value, edges: &[(&str, &str)]) -> Arc<Document> {
    let edges: Vec<Value> = edges.iter().enumerate().map(|(i, (f, t))| json!({ "id": format!("e{}", i + 1), "from": f, "to": t })).collect();
    Arc::new(doc::from_value(json!({ "schema": 1, "id": "wf-rea", "name": "rea", "nodes": nodes, "edges": edges })).expect("a valid document shape"))
}
fn n(id: &str, kind: &str) -> Value { json!({ "id": id, "kind": kind }) }
fn nc(id: &str, kind: &str, config: Value) -> Value { json!({ "id": id, "kind": kind, "config": config }) }
async fn go(d: &Arc<Document>, host: &Arc<Re>, prompt: &str) -> RunReport {
    let h: Arc<dyn WorkflowHost> = host.clone();
    Engine::with_builtins().start(d.clone(), RunInput::prompt(prompt), h, RunOptions::default()).await
}
fn end_of<'a>(r: &'a RunReport, key: &str) -> &'a Value { r.log.iter().rev().find(|e| e["type"] == "node_end" && e["key"] == key).expect("a node_end") }
fn one(config: Value, port_out: &str) -> Arc<Document> {
    wf(json!([n("s", "start"), nc("x", "rea", config), n("e", "end")]), &[("s.prompt", "x.args"), (&format!("x.{port_out}"), "e.result")])
}

#[tokio::test]
async fn a_rea_step_starts_a_copy_runs_reas_tool_and_throws_the_copy_away() {
    let host = Arc::new(Re::default());
    let d = one(json!({ "tool": "echo", "args": { "file": "{{prompt}}" }, "purpose": "look at {{prompt}}" }), "text");
    let r = go(&d, &host, "app.apk").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!("echo {\"file\":\"app.apk\"}")), "the args are filled from the run");
    assert_eq!(host.names(), ["re_start", "re_call", "re_stop"]);
    assert_eq!(host.calls.lock().unwrap()[0].1["purpose"], "look at app.apk");
    assert_eq!(host.running(), 0, "the copy is deleted when the node is done");
    assert!(end_of(&r, "x")["outputs"].get("copy").is_none(), "a thrown-away copy is not handed on");
}

#[tokio::test]
async fn a_queued_copy_is_waited_for_and_a_full_queue_gives_up_and_cleans_up() {
    let host = Arc::new(Re::queued(1));
    let r = go(&one(json!({ "tool": "echo" }), "result"), &host, "x").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(host.names(), ["re_start", "re_status", "re_call", "re_stop"]);
    let host = Arc::new(Re::queued(100));
    let r = go(&one(json!({ "tool": "echo", "queueSec": 2 }), "result"), &host, "x").await;
    assert_eq!(r.status, "failed");
    assert_eq!(r.error.unwrap()["code"], "rea.queue");
    assert_eq!(host.running(), 0, "a copy that never ran is not left in the queue");
}

#[tokio::test]
async fn files_go_in_the_command_runs_and_files_come_out() {
    let host = Arc::new(Re::default());
    let d = one(json!({ "files": [{ "path": "in/notes.txt", "text": "about {{prompt}}" }], "exec": "strings in/notes.txt", "get": ["in/notes.txt"] }), "files");
    let r = go(&d, &host, "the sample").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result.clone().unwrap()["in/notes.txt"]["text"], "about the sample");
    assert_eq!(end_of(&r, "x")["outputs"]["output"]["stdout"], "ran strings in/notes.txt");
    assert_eq!(host.names(), ["re_start", "re_put", "re_exec", "re_get", "re_stop"]);
}

#[tokio::test]
async fn a_failing_command_or_rea_tool_is_the_nodes_error_and_the_copy_is_still_deleted() {
    let host = Arc::new(Re::default());
    let r = go(&one(json!({ "exec": "false" }), "output"), &host, "x").await;
    assert_eq!(r.error.unwrap()["code"], "rea.exec");
    assert_eq!(host.running(), 0);
    let host = Arc::new(Re::default());
    let r = go(&one(json!({ "exec": "false", "failOnExit": false }), "output"), &host, "x").await;
    assert_eq!(r.status, "ok", "failOnExit: false hands the exit code on instead");
    let host = Arc::new(Re::default());
    let r = go(&one(json!({ "tool": "broken", "keep": true }), "result"), &host, "x").await;
    let e = r.error.unwrap();
    assert_eq!(e["code"], "rea.tool");
    assert!(e["message"].as_str().unwrap().contains("no such binary"), "{e}");
    assert_eq!(host.running(), 0, "even a kept copy is deleted when its own node failed");
}

#[tokio::test]
async fn a_kept_copy_is_handed_on_used_by_the_next_node_and_stopped_by_the_last() {
    let host = Arc::new(Re::default());
    let d = wf(json!([n("s", "start"),
        nc("a", "rea", json!({ "files": [{ "path": "in/x.bin", "text": "{{prompt}}" }], "keep": true })),
        nc("b", "rea", json!({ "tool": "echo", "args": { "path": "in/x.bin" } })),
        nc("c", "rea", json!({ "stop": true })),
        n("e", "end")]),
        &[("s.prompt", "a.args"), ("a.copy", "b.copy"), ("b.copy", "c.copy"), ("b.text", "e.result")]);
    let r = go(&d, &host, "bytes").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(host.names(), ["re_put", "re_call", "re_stop"].iter().fold(vec!["re_start".to_string()], |mut v, x| { v.push(x.to_string()); v }));
    assert_eq!(end_of(&r, "a")["outputs"]["copy"], "re-1");
    assert_eq!(end_of(&r, "b")["outputs"]["copy"], "re-1", "a copy handed in stays running and is handed on");
    assert!(end_of(&r, "c")["outputs"].get("copy").is_none());
    assert_eq!(host.running(), 0, "the stop node deleted it");
}

#[tokio::test]
async fn a_host_or_scope_without_the_re_tools_refuses_and_a_step_that_does_nothing_is_a_config_error() {
    let host = Arc::new(Re { refuse: true, ..Re::default() });
    let r = go(&one(json!({ "tool": "echo" }), "result"), &host, "x").await;
    assert_eq!(r.error.unwrap()["code"], "engine.tool-denied", "a workflow cannot grant itself REA");
    let host = Arc::new(Re::default());
    let r = go(&one(json!({ "purpose": "nothing" }), "result"), &host, "x").await;
    assert_eq!(r.error.unwrap()["code"], "engine.config");
    assert!(host.names().is_empty(), "nothing was started");
}
