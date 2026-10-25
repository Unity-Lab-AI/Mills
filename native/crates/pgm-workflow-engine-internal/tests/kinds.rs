//! The built-in kinds' spec checks (plans/hyper-workflows.md section 16, T-201: R418 to R423), with the fake host and
//! no network. bridge/test/regress.js runs this file and maps the test names (`r418_...`) back to the checks.
use pgm_workflow_engine_internal::fake::{FakeHost, Reply};
use pgm_workflow_engine_internal::model::{doc, Document};
use pgm_workflow_engine_internal::{Engine, RunInput, RunOptions, RunReport, WorkflowHost};
use serde_json::{json, Value};
use std::sync::Arc;

fn wf(nodes: Value, edges: &[(&str, &str)], extra: Value) -> Arc<Document> {
    let edges: Vec<Value> = edges.iter().enumerate().map(|(i, (f, t))| json!({ "id": format!("e{}", i + 1), "from": f, "to": t })).collect();
    let mut v = json!({ "schema": 1, "id": "wf-test", "name": "test", "nodes": nodes, "edges": edges });
    if let (Some(o), Value::Object(x)) = (v.as_object_mut(), extra) { for (k, val) in x { o.insert(k, val); } }
    Arc::new(doc::from_value(v).expect("a valid document shape"))
}
fn n(id: &str, kind: &str) -> Value { json!({ "id": id, "kind": kind }) }
fn nc(id: &str, kind: &str, config: Value) -> Value { json!({ "id": id, "kind": kind, "config": config }) }
fn h(host: &Arc<FakeHost>) -> Arc<dyn WorkflowHost> { host.clone() }
fn engine() -> Engine { Engine::with_builtins() }
async fn go(d: &Arc<Document>, host: &Arc<FakeHost>, prompt: &str) -> RunReport { engine().start(d.clone(), RunInput::prompt(prompt), h(host), RunOptions::default()).await }
fn one(kind: &str, config: Value, port_in: &str, port_out: &str) -> Arc<Document> {
    wf(json!([n("s", "start"), nc("x", kind, config), n("e", "end")]), &[("s.prompt", &format!("x.{port_in}")), (&format!("x.{port_out}"), "e.result")], json!({}))
}
fn end_of<'a>(r: &'a RunReport, key: &str) -> &'a Value { r.log.iter().rev().find(|e| e["type"] == "node_end" && e["key"] == key).expect("a node_end") }

// ---------------------------------------------------------------- R418 prompt, chat, summarise

#[tokio::test]
async fn r418_prompt_templates_fill_from_wires_and_a_missing_value_is_an_error() {
    let host = FakeHost::new();
    let d = wf(json!([n("s", "start"), nc("x", "prompt", json!({ "system": "You write for {{inputs.audience}}.", "user": "Topic: {{node.s.out.prompt}}" })), n("e", "end")]),
        &[("s.prompt", "x.in"), ("x.text", "e.result")], json!({ "inputs": [{ "name": "audience", "type": "text", "default": "the team" }] }));
    let r = go(&d, &host, "caching").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(host.calls_of("x")[0].prompt, "You write for the team.\nTopic: caching");
    let bad = one("prompt", json!({ "user": "{{inputs.nope}}" }), "in", "text");
    let r = go(&bad, &FakeHost::new(), "x").await;
    assert_eq!(r.status, "failed");
    assert_eq!(r.error.unwrap()["code"], "engine.template");
}

#[tokio::test]
async fn r418_a_bad_json_answer_is_asked_again() {
    let host = FakeHost::new();
    host.replies("x", vec![Reply::text("sure! here you go"), Reply::text("```json\n{\"a\": 1}\n```")]);
    let r = go(&one("prompt", json!({ "responseFormat": "json" }), "in", "json"), &host, "x").await;
    assert_eq!(r.result, Some(json!({ "a": 1 })), "{r:?}");
    assert_eq!(host.calls_of("x").len(), 2);
    let host = FakeHost::new();
    host.replies("x", vec![Reply::text("no"), Reply::text("still no")]);
    let r = go(&one("prompt", json!({ "responseFormat": "json" }), "in", "json"), &host, "x").await;
    assert_eq!(r.error.unwrap()["code"], "engine.bad-json");
}

#[tokio::test]
async fn r418_chat_stops_at_max_turns_and_at_stop_when() {
    let host = FakeHost::new();
    let d = |cfg: Value| wf(json!([n("s", "start"), nc("x", "chat", cfg), n("e", "end")]), &[("s.prompt", "x.in"), ("x.stopped", "e.result")], json!({}));
    let r = go(&d(json!({ "maxTurns": 3 })), &host, "draft it").await;
    assert_eq!(r.result, Some(json!("maxTurns")), "{r:?}");
    assert_eq!(end_of(&r, "x")["outputs"]["turns"], 3);
    assert_eq!((host.calls_of("x").len(), host.calls_of("x#partner").len()), (3, 2));
    let host = FakeHost::new();
    host.replies("x", vec![Reply::text("first try"), Reply::text("all good, FINISHED")]);
    let r = go(&d(json!({ "maxTurns": 5, "stopWhen": "FINISHED" })), &host, "draft it").await;
    assert_eq!(r.result, Some(json!("stopWhen")));
    assert_eq!(end_of(&r, "x")["outputs"]["turns"], 2);
    assert_eq!(end_of(&r, "x")["outputs"]["messages"].as_array().unwrap().len(), 4, "opening, answer, partner, answer");
    let host = FakeHost::new();
    host.replies("x", vec![Reply::text("short"), Reply::text("a much longer answer")]);
    let r = go(&d(json!({ "maxTurns": 5, "stopWhen": "len(value) > 10" })), &host, "x").await;
    assert_eq!(end_of(&r, "x")["outputs"]["turns"], 2, "a condition works too");
}

#[tokio::test]
async fn r418_summarise_keeps_the_keep_items_and_cuts_long_texts() {
    let host = FakeHost::new();
    host.reply("x", Reply::text("- the build was changed"));
    let d = wf(json!([n("s", "start"), nc("x", "summarise", json!({ "target": { "bullets": 3 }, "keep": ["src/main.rs", "not in the text"] })), n("e", "end")]),
        &[("s.prompt", "x.text"), ("x.summary", "e.result")], json!({}));
    let r = go(&d, &host, "We changed the build in src/main.rs and the docs.").await;
    let s = r.result.clone().unwrap();
    assert!(s.as_str().unwrap().ends_with("Kept: src/main.rs"), "{s}");
    assert_eq!(end_of(&r, "x")["outputs"]["kept"], json!(["src/main.rs"]));
    assert!(host.calls_of("x")[0].prompt.contains("at most 3 bullet points"));
    let host = FakeHost::new();
    let para = "word ".repeat(30);
    let long = format!("{para}\n\n{para}\n\n{para}");
    let d = wf(json!([n("s", "start"), nc("x", "summarise", json!({ "pieceChars": 200 })), n("e", "end")]), &[("s.prompt", "x.text"), ("x.summary", "e.result")], json!({}));
    let r = go(&d, &host, &long).await;
    assert_eq!(r.status, "ok");
    assert_eq!(host.calls_of("x#part1").len() + host.calls_of("x#part2").len() + host.calls_of("x#part3").len(), 3);
    assert_eq!(host.calls_of("x#merge").len(), 1);
}

// ---------------------------------------------------------------- R419 tool

#[tokio::test]
async fn r419_a_tool_outside_the_policy_is_denied() {
    let host = FakeHost::new();
    host.allow_tools(&["board.list"]);
    let ok = wf(json!([n("s", "start"), nc("x", "tool", json!({ "tool": "board.list" })), n("e", "end")]), &[("s.inputs", "x.args"), ("x.result", "e.result")], json!({}));
    assert_eq!(go(&ok, &host, "x").await.status, "ok");
    let no = wf(json!([n("s", "start"), nc("x", "tool", json!({ "tool": "notes.delete" })), n("e", "end")]), &[("s.inputs", "x.args"), ("x.result", "e.result")], json!({}));
    let r = go(&no, &host, "x").await;
    assert_eq!(r.error.unwrap()["code"], "engine.tool-denied", "the host refuses what the scope does not allow");
}

#[tokio::test]
async fn r419_a_custom_http_call_maps_status_and_path_and_never_logs_a_secret() {
    let host = FakeHost::new();
    host.secret("API_KEY", "s3cr3t-value");
    host.http_reply(201, r#"{"data":{"id":7,"name":"x"}}"#);
    let http = json!({ "method": "POST", "url": "https://api.example.com/items?q={{inputs.prompt}}", "headers": { "authorization": "Bearer {{secret:API_KEY}}" }, "body": { "title": "{{inputs.prompt}}" }, "expect": { "status": 201, "path": "data.id" } });
    let d = wf(json!([n("s", "start"), nc("x", "tool", json!({ "http": http })), n("e", "end")]), &[("s.inputs", "x.args"), ("x.result", "e.result")], json!({}));
    let r = go(&d, &host, "hello").await;
    assert_eq!(r.result, Some(json!(7)), "{r:?}");
    let sent = host.http_sent();
    assert_eq!(sent[0].url, "https://api.example.com/items?q=hello");
    assert_eq!(sent[0].headers[0], ("authorization".to_string(), "Bearer s3cr3t-value".to_string()));
    assert_eq!(sent[0].body.as_deref(), Some(r#"{"title":"hello"}"#));
    let all = serde_json::to_string(&r.log).unwrap();
    assert!(!all.contains("s3cr3t-value"), "the secret is never in the log");
    host.http_reply(500, "oops");
    let r = go(&d, &host, "hello").await;
    assert_eq!(r.error.unwrap()["code"], "engine.http-status");
    let leak = wf(json!([n("s", "start"), nc("x", "prompt", json!({ "user": "{{secret:API_KEY}}" })), n("e", "end")]), &[("s.prompt", "x.in"), ("x.text", "e.result")], json!({}));
    assert_eq!(go(&leak, &host, "x").await.error.unwrap()["code"], "engine.template", "a prompt cannot read a secret");
}

// ---------------------------------------------------------------- R420 script

#[tokio::test]
async fn r420_the_transform_language_computes_and_reaches_nothing_else() {
    let d = |code: &str, outs: Value, extra: Value| {
        let mut cfg = json!({ "lang": "transform", "code": code, "outputs": outs });
        if let (Some(o), Value::Object(x)) = (cfg.as_object_mut(), extra) { for (k, v) in x { o.insert(k, v); } }
        wf(json!([n("s", "start"), nc("x", "script", cfg), n("e", "end")]), &[("s.inputs", "x.in"), ("x.big", "e.result")], json!({}))
    };
    let input = RunInput::prompt("x").input("a", json!(2)).input("b", json!(3));
    let r = engine().start(d("s = input.a + input.b\nbig = s > 4 and len(inputs.prompt) == 1", json!(["s", "big"]), json!({})), input.clone(), h(&FakeHost::new()), RunOptions::default()).await;
    assert_eq!(r.result, Some(json!(true)), "{r:?}");
    assert_eq!(end_of(&r, "x")["outputs"]["s"], 5);
    for code in ["big = now()", "big = read('/etc/passwd')", "big = fetch('https://x')"] {
        let r = engine().start(d(code, json!(["big"]), json!({})), input.clone(), h(&FakeHost::new()), RunOptions::default()).await;
        assert_eq!(r.error.unwrap()["code"], "engine.script", "{code}: there is no such function");
    }
    let r = engine().start(d("big = input.a + input.b + input.a + input.b", json!(["big"]), json!({ "maxSteps": 3 })), input.clone(), h(&FakeHost::new()), RunOptions::default()).await;
    assert_eq!(r.error.unwrap()["code"], "engine.script-steps");
    let r = engine().start(d("big = 1", json!(["big"]), json!({ "lang": "python" })), input, h(&FakeHost::new()), RunOptions::default()).await;
    assert_eq!(r.error.unwrap()["code"], "engine.script-lang");
}

// ---------------------------------------------------------------- R421 model choice

#[tokio::test]
async fn r421_node_then_section_inner_first_then_workflow_default_then_fallback() {
    let host = FakeHost::new();
    let nodes = json!([n("s", "start"), nc("own", "prompt", json!({ "model": { "via": "ollama", "model": "n" } })), n("inner", "prompt"), n("outer", "prompt"), n("free", "prompt"),
        nc("j", "join", json!({})), n("e", "end")]);
    let edges = [("s.prompt", "own.in"), ("s.prompt", "inner.in"), ("s.prompt", "outer.in"), ("s.prompt", "free.in"), ("own.text", "j.lines"), ("inner.text", "j.lines"), ("outer.text", "j.lines"), ("free.text", "j.lines"), ("j.result", "e.result")];
    let d = wf(nodes, &edges, json!({
        "defaults": { "model": { "via": "host", "provider": "claude", "model": "d" } },
        "sections": [ { "id": "big", "nodes": ["own", "inner", "outer"], "model": { "via": "openrouter", "model": "s-big" } }, { "id": "small", "nodes": ["inner"], "model": { "via": "openrouter", "model": "s-small" } } ],
    }));
    let r = go(&d, &host, "x").await;
    assert_eq!(r.status, "ok", "{r:?}");
    let model = |id: &str| host.calls_of(id)[0].model.clone().unwrap()["model"].clone();
    assert_eq!((model("own"), model("inner"), model("outer"), model("free")), (json!("n"), json!("s-small"), json!("s-big"), json!("d")));
    let host = FakeHost::new();
    host.replies("x", vec![Reply::fail("model.unreachable"), Reply::text("from b")]);
    let d = one("prompt", json!({ "model": { "via": "host", "model": "a", "fallback": [{ "via": "openrouter", "model": "b" }] } }), "in", "text");
    let r = go(&d, &host, "x").await;
    assert_eq!(r.result, Some(json!("from b")), "{r:?}");
    assert_eq!(host.calls_of("x")[1].model.clone().unwrap()["model"], "b");
    let host = FakeHost::new();
    host.replies("x", vec![Reply::fail("model.unreachable"), Reply::fail("model.rate-limited")]);
    assert_eq!(go(&d, &host, "x").await.error.unwrap()["code"], "engine.model-unreachable");
}

// ---------------------------------------------------------------- R422 sub-workflow, agent, orchestrate

fn helper() -> Document {
    doc::from_value(json!({ "schema": 1, "id": "wf-helper", "name": "helper", "version": 3, "nodes": [n("s", "start"), n("p", "prompt"), nc("e", "end", json!({ "name": "helped" }))],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "p.in" }, { "id": "e2", "from": "p.text", "to": "e.result" }] })).unwrap()
}

#[tokio::test]
async fn r422_sub_workflow_runs_a_pinned_child_and_links_both_logs() {
    let host = FakeHost::new();
    host.workflow(helper());
    let d = wf(json!([n("s", "start"), nc("x", "sub-workflow", json!({ "name": "helper", "range": "3" })), n("e", "end")]), &[("s.prompt", "x.in"), ("x.result", "e.result")], json!({}));
    let r = go(&d, &host, "hi").await;
    assert_eq!(r.status, "ok", "{r:?}");
    assert_eq!(r.result, Some(json!("ai:p:hi")));
    let x = end_of(&r, "x");
    assert_eq!(x["outputs"]["outcome"], "helped");
    let child = x["child"]["run"].as_str().unwrap().to_string();
    assert_eq!(x["child"]["version"], 3);
    let child_start = host.log().into_iter().find(|e| e["type"] == "run_start" && e["run"] == json!(child)).expect("the child's own log");
    assert_eq!(child_start["parent"]["run"], json!(r.run));
    assert_eq!(child_start["parent"]["node"], "x");
    // a workflow that calls itself stops at the depth limit
    let me = doc::from_value(json!({ "schema": 1, "id": "wf-me", "name": "me", "nodes": [n("s", "start"), nc("x", "sub-workflow", json!({ "name": "me" })), n("e", "end")],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "x.in" }, { "id": "e2", "from": "x.result", "to": "e.result" }] })).unwrap();
    let host = FakeHost::new();
    host.workflow(me.clone());
    let r = engine().start(Arc::new(me), RunInput::prompt("x"), h(&host), RunOptions::default()).await;
    assert_eq!(r.status, "failed");
    assert!(r.error.unwrap()["message"].as_str().unwrap().contains("deep"), "refused past the depth limit");
    let r = go(&wf(json!([n("s", "start"), nc("x", "sub-workflow", json!({ "name": "nobody" })), n("e", "end")]), &[("s.prompt", "x.in"), ("x.result", "e.result")], json!({})), &FakeHost::new(), "x").await;
    assert_eq!(r.error.unwrap()["code"], "engine.no-workflow");
}

#[tokio::test]
async fn r422_agent_waits_for_the_final_answer() {
    let host = FakeHost::new();
    host.agent_reply(json!({ "text": "did it", "result": { "files": 2 } }));
    let d = wf(json!([n("s", "start"), nc("x", "agent", json!({ "tools": ["read"], "timeoutSec": 600 })), n("e", "end")]), &[("s.prompt", "x.task"), ("x.result", "e.result")], json!({}));
    let r = engine().start(d, RunInput::prompt("fix the build").project("VibeRoller"), h(&host), RunOptions::default()).await;
    assert_eq!(r.result, Some(json!({ "files": 2 })), "{r:?}");
    let spec = host.agents.lock().unwrap()[0].clone();
    assert_eq!((spec["task"].as_str(), spec["project"].as_str(), spec["tools"].clone()), (Some("fix the build"), Some("VibeRoller"), json!(["read"])));
}

#[tokio::test]
async fn r422_orchestrate_plans_fans_out_and_reviews_within_its_limit() {
    let host = FakeHost::new();
    host.reply("x#plan", Reply::text(r#"["write the tests", "write the code"]"#));
    host.reply("x#review", Reply::text(r#"{"verdict": "done", "notes": "fine"}"#));
    let d = one("orchestrate", json!({ "maxRounds": 2 }), "goal", "result");
    let r = go(&d, &host, "add a feature").await;
    assert_eq!(r.status, "ok", "{r:?}");
    let res = r.result.unwrap();
    assert_eq!(res["results"].as_array().unwrap().len(), 2);
    assert_eq!(res["verdict"], "done");
    assert_eq!(host.calls_of("x#work1").len() + host.calls_of("x#work2").len(), 2, "one line per subtask");
    let host = FakeHost::new();
    host.reply("x#plan", Reply::text(r#"["a"]"#));
    for _ in 0..5 { host.reply("x#review", Reply::text(r#"{"verdict": "again", "notes": "more"}"#)); host.reply("x#plan", Reply::text(r#"["a"]"#)); }
    let r = go(&d, &host, "x").await;
    assert_eq!(r.result.unwrap()["rounds"], 2, "the review loop is bounded by maxRounds");
}

// ---------------------------------------------------------------- R423 jev

fn decision(cfg: Value) -> Arc<Document> {
    wf(json!([n("s", "start"), nc("j", "jev", cfg), nc("e1", "end", json!({ "name": "shipped" })), nc("e2", "end", json!({ "name": "held" }))]),
        &[("s.prompt", "j.context"), ("j.ship", "e1.result"), ("j.hold", "e2.result")], json!({}))
}
fn pick(c: &str, conf: f64) -> Reply { Reply::text(&format!(r#"{{"choice": "{c}", "reason": "because", "confidence": {conf}}}"#)) }

#[tokio::test]
async fn r423_jev_picks_one_choice_port() {
    let host = FakeHost::new();
    host.reply("j", pick("hold", 0.9));
    let r = go(&decision(json!({ "question": "Ship it?", "choices": ["ship", "hold"] })), &host, "the diff").await;
    assert_eq!((r.status.as_str(), r.outcome.as_deref()), ("ok", Some("held")), "{r:?}");
    assert_eq!(r.result, Some(json!("the diff")), "the chosen port carries the context");
    assert_eq!(end_of(&r, "j")["outputs"]["confidence"], 0.9);
    let d = wf(json!([n("s", "start"), nc("j", "jev", json!({ "mode": "yes-no", "question": "ok?" })), nc("y", "end", json!({ "name": "yes" })), nc("no", "end", json!({ "name": "no" }))]),
        &[("s.prompt", "j.context"), ("j.yes", "y.result"), ("j.no", "no.result")], json!({}));
    let host = FakeHost::new();
    host.reply("j", pick("yes", 1.0));
    assert_eq!(go(&d, &host, "x").await.outcome.as_deref(), Some("yes"));
}

#[tokio::test]
async fn r423_low_confidence_follows_on_low_confidence() {
    let base = json!({ "question": "Ship it?", "choices": ["ship", "hold"], "minConfidence": 0.7, "default": "hold" });
    let with = |k: &str| { let mut c = base.clone(); c["onLowConfidence"] = json!(k); decision(c) };
    let host = FakeHost::new();
    host.reply("j", pick("ship", 0.4));
    assert_eq!(go(&with("default"), &host, "x").await.outcome.as_deref(), Some("held"));
    let host = FakeHost::new();
    host.reply("j", pick("ship", 0.4));
    assert_eq!(go(&with("error"), &host, "x").await.error.unwrap()["code"], "engine.low-confidence");
    let host = FakeHost::new();
    host.reply("j", pick("ship", 0.4)).reply("j#second", pick("hold", 0.95));
    assert_eq!(go(&with("second-opinion"), &host, "x").await.outcome.as_deref(), Some("held"));
    let host = FakeHost::new();
    host.reply("j", pick("ship", 0.4));
    let d = with("ask-person");
    let r = go(&d, &host, "x").await;
    assert_eq!((r.status.as_str(), r.waiting[0]["what"].as_str()), ("waiting", Some("decision")), "{r:?}");
    assert_eq!(host.asks.lock().unwrap()[0].questions[0]["options"], json!(["ship", "hold"]));
    let r = engine().answer(d, r.log, "j", json!(["hold"]), "user", h(&host), RunOptions::default()).await;
    assert_eq!(r.outcome.as_deref(), Some("held"), "{r:?}");
    assert_eq!(host.calls_of("j").len(), 1, "the model was not asked again after the person decided");
}

#[tokio::test]
async fn r423_off_shape_is_retried_then_an_error_and_votes_take_the_majority() {
    let host = FakeHost::new();
    host.replies("j", vec![Reply::text("I think ship"), pick("ship", 0.8)]);
    assert_eq!(go(&decision(json!({ "choices": ["ship", "hold"] })), &host, "x").await.outcome.as_deref(), Some("shipped"));
    let host = FakeHost::new();
    host.replies("j", vec![Reply::text("I think ship"), pick("maybe", 0.8)]);
    assert_eq!(go(&decision(json!({ "choices": ["ship", "hold"] })), &host, "x").await.error.unwrap()["code"], "engine.jev-shape");
    let host = FakeHost::new();
    host.reply("j#vote1", pick("hold", 0.9)).reply("j#vote2", pick("ship", 0.9)).reply("j#vote3", pick("hold", 0.9));
    let r = go(&decision(json!({ "choices": ["ship", "hold"], "votes": 3 })), &host, "x").await;
    assert_eq!(r.outcome.as_deref(), Some("held"), "{r:?}");
    assert!(end_of(&r, "j")["outputs"]["reason"].as_str().unwrap().contains("2 of 3 votes"));
}

#[tokio::test]
async fn r423_the_decision_model_through_openrouter_ollama_and_host() {
    for via in ["openrouter", "ollama", "host"] {
        let host = FakeHost::new();
        host.reply("j", pick("ship", 0.9));
        let r = go(&decision(json!({ "choices": ["ship", "hold"], "model": { "via": via, "model": "decision-small" } })), &host, "x").await;
        assert_eq!(r.outcome.as_deref(), Some("shipped"), "{via}: {r:?}");
        assert_eq!(host.calls_of("j")[0].model.clone().unwrap()["via"], via);
    }
}

#[tokio::test]
async fn r418_a_saved_prompt_fills_a_template() {
    let host = FakeHost::new();
    host.prompt("release-style", "Write short release notes.");
    let d = wf(json!([n("s", "start"), nc("x", "prompt", json!({ "system": "{{prompt:release-style}}", "user": "{{inputs.prompt}}" })), n("e", "end")]), &[("s.prompt", "x.in"), ("x.text", "e.result")], json!({}));
    assert_eq!(go(&d, &host, "v5").await.status, "ok");
    assert_eq!(host.calls_of("x")[0].prompt, "Write short release notes.\nv5");
    let r = go(&wf(json!([n("s", "start"), nc("x", "prompt", json!({ "system": "{{prompt:nobody}}" })), n("e", "end")]), &[("s.prompt", "x.in"), ("x.text", "e.result")], json!({})), &host, "v5").await;
    assert_eq!(r.error.unwrap()["code"], "engine.template");
}

#[tokio::test]
async fn r422_orchestrate_picks_workers_with_a_pgm_orchestrator_profile() {
    let profile = pgm_orchestrator_internal::profile::starter_profile();
    let routes: Vec<String> = profile.workers.iter().map(|w| w.route.clone()).collect();
    let host = FakeHost::new();
    host.reply("x#plan", Reply::text(r#"[{"task": "research how the cache works", "kind": "research"}]"#));
    host.reply("x#review", Reply::text(r#"{"verdict": "done"}"#));
    let d = one("orchestrate", json!({ "workers": serde_json::to_value(&profile).unwrap() }), "goal", "result");
    let r = go(&d, &host, "speed it up").await;
    assert_eq!(r.status, "ok", "{r:?}");
    let m = host.calls_of("x#work1")[0].model.clone().expect("a worker route was chosen");
    assert!(routes.contains(&m["route"].as_str().unwrap().to_string()), "{m} is one of the profile's routes");
}

#[tokio::test]
async fn r423_the_decision_model_slot_openrouter_then_a_local_model() {
    // T-210: one decision node; the scope's decision model is JEV through OpenRouter, with a local model (Ollama) behind it
    let slot = json!({ "via": "openrouter", "model": "jev-1", "fallback": [{ "via": "ollama", "model": "laya" }] });
    let d = decision(json!({ "question": "Ship it?", "choices": ["ship", "hold"] }));
    let host = FakeHost::new();
    host.decision_model(slot.clone());
    host.reply("j", pick("ship", 0.9));
    assert_eq!(go(&d, &host, "x").await.outcome.as_deref(), Some("shipped"));
    assert_eq!(host.calls_of("j")[0].model.clone().unwrap(), json!({ "via": "openrouter", "model": "jev-1" }), "JEV through OpenRouter first");
    let host = FakeHost::new();
    host.decision_model(slot.clone());
    host.replies("j", vec![Reply::fail("model.unreachable"), pick("hold", 0.8)]);
    assert_eq!(go(&d, &host, "x").await.outcome.as_deref(), Some("held"), "the same node, OpenRouter down: the local model decides");
    assert_eq!(host.calls_of("j")[1].model.clone().unwrap(), json!({ "via": "ollama", "model": "laya" }));
    let host = FakeHost::new();
    host.decision_model(slot);
    host.reply("j", pick("ship", 0.9));
    go(&decision(json!({ "choices": ["ship", "hold"], "model": { "via": "host", "model": "mine" } })), &host, "x").await;
    assert_eq!(host.calls_of("j")[0].model.clone().unwrap()["model"], "mine", "a node's own model wins over the slot");
}
