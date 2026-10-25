//! The done-when checks of T-199 (spec section 16): R401 to R406. Test names start with the check's id
//! (`r401_...`), which is how bridge/test/regress.js maps `cargo test` output back to the checks.
use pgm_workflow_internal::*;
use serde_json::{json, Value};

const FIXTURE: &str = include_str!("fixtures/release-notes.json");

fn reg() -> DefaultRegistry { DefaultRegistry::new() }
fn check(d: &Document) -> Report { validate(d, &Options::new(&reg())) }
fn check_v(v: Value) -> Report { check(&doc::from_value(v).expect("a document")) }

/// start -> end, nothing else: the smallest valid workflow.
fn base() -> Value {
    json!({
        "schema": 1, "id": "wf-test", "name": "t",
        "nodes": [ { "id": "n0", "kind": "start", "kindVersion": 1 }, { "id": "n9", "kind": "end", "kindVersion": 1, "config": { "name": "done" } } ],
        "edges": [ { "id": "e0", "from": "n0.prompt", "to": "n9.result" } ]
    })
}
fn with_nodes(extra: Vec<Value>, edges: Vec<Value>) -> Value {
    let mut v = base();
    v["nodes"].as_array_mut().unwrap().extend(extra);
    v["edges"].as_array_mut().unwrap().extend(edges);
    v
}
fn node(id: &str, kind: &str, config: Value) -> Value { json!({ "id": id, "kind": kind, "kindVersion": 1, "config": config }) }
fn edge(id: &str, from: &str, to: &str) -> Value { json!({ "id": id, "from": from, "to": to }) }

struct Scope;
impl ScopeInfo for Scope {
    fn law_visible(&self, id: &str) -> bool { ["law-tone", "grp-safety", "law-cite", "law-no-secrets"].contains(&id) }
    fn prompt_visible(&self, id: &str) -> bool { id == "release-style" }
    fn model_reachable(&self, m: &Value) -> bool { m.get("via").and_then(Value::as_str) != Some("ollama") }
}

// ---------------------------------------------------------------- R401

#[test]
fn r401_round_trip_keeps_unknown_fields() {
    let d = doc::parse(FIXTURE).unwrap();
    // the fixture is in canonical form: parse then write gives the same bytes
    assert_eq!(doc::write(&d) + "\n", FIXTURE);
    // and the same JSON, field for field
    assert_eq!(doc::to_value(&d), serde_json::from_str::<Value>(FIXTURE).unwrap());
    // unknown fields survived at every level
    assert_eq!(d.extra["x-editor"], json!({ "zoom": 1.5, "grid": true }));
    assert_eq!(d.node("n3").unwrap().extra["x-editor-note"], json!({ "seen": true }));
    assert_eq!(d.edge("e1").unwrap().extra["x-color"], "#fff");
    assert_eq!(d.sections.as_ref().unwrap()[0].extra["x-collapsed"], false);
    assert_eq!(d.nodes.len(), 9);
    // write -> parse -> write is stable
    let again = doc::parse(&doc::write(&d)).unwrap();
    assert_eq!(again, d);
    assert_eq!(doc::write(&again), doc::write(&d));
    // a messier layout of the same document gives the canonical text
    let compact = serde_json::to_string(&serde_json::from_str::<Value>(FIXTURE).unwrap()).unwrap();
    assert_eq!(doc::canonical(&compact).unwrap() + "\n", FIXTURE);
    // absent stays absent, [] stays []
    let mut v = base();
    v["labels"] = json!([]);
    let d = doc::from_value(v.clone()).unwrap();
    assert_eq!(doc::to_value(&d), v);
}

#[test]
fn r401_older_schema_is_migrated_on_read() {
    let old = json!({
        "id": "wf-old", "name": "old", "flavour": "kept",
        "nodes": [ { "id": "a", "type": "start" }, { "id": "b", "type": "end" } ],
        "connections": [ { "id": "e1", "from": "a.prompt", "to": "b.result" } ]
    });
    let text = old.to_string();
    let d = doc::parse(&text).unwrap();
    assert_eq!(d.schema, SCHEMA);
    assert_eq!((d.nodes[0].kind.as_str(), d.nodes[1].kind.as_str(), d.edges.len()), ("start", "end", 1));
    assert_eq!(d.extra["flavour"], "kept");
    assert!(d.extra.get("connections").is_none());
    assert!(check(&d).is_valid());
    // an explicit schema 0 is the same
    let mut zero = old.clone();
    zero["schema"] = json!(0);
    assert_eq!(doc::from_value(zero).unwrap().edges.len(), 1);
    // reading never rewrites: the text we were given is still what it was (migration works on a copy)
    assert_eq!(text, old.to_string());
}

#[test]
fn r401_newer_schema_is_refused_with_a_message() {
    let mut v = base();
    v["schema"] = json!(SCHEMA + 1);
    v["somethingNew"] = json!(true);
    let e = doc::from_value(v).unwrap_err();
    assert_eq!(e.code(), 422);
    let m = e.to_string();
    assert!(m.contains("schema 2") && m.contains("newer") && m.contains("not changed"), "{m}");
    for (bad, want) in [("[1]", "object"), (r#"{"schema":"x"}"#, "whole number"), ("{nope", "not valid JSON"), (r#"{"schema":1,"id":"a"}"#, "not a workflow document")] {
        let m = doc::parse(bad).unwrap_err().to_string();
        assert!(m.contains(want), "{bad}: {m}");
    }
    let huge = format!(r#"{{"schema":1,"pad":"{}"}}"#, "x".repeat(MAX_BYTES));
    assert_eq!(doc::parse(&huge).unwrap_err().code(), 413);
}

#[test]
fn r401_the_fixture_is_valid() {
    let d = doc::parse(FIXTURE).unwrap();
    let r = check(&d);
    assert!(r.issues.is_empty(), "{:#?}", r.issues);
    // with a scope that knows the laws and the prompt, and reaches every model but ollama's: only that warning
    let r = validate(&d, &Options::new(&reg()).scope(&Scope));
    assert!(r.is_valid(), "{:#?}", r.issues);
    assert_eq!(r.warnings().map(|i| i.code.as_str()).collect::<Vec<_>>(), ["model.unreachable"]);
    assert_eq!(r.warnings().next().unwrap().node.as_deref(), Some("n4"));
}

// ---------------------------------------------------------------- R402

fn only(r: &Report, code: &str) -> Issue {
    let hits = r.with_code(code);
    assert_eq!(hits.len(), 1, "{code}: {:#?}", r.issues);
    hits[0].clone()
}

#[test]
fn r402_the_base_is_valid() {
    let r = check_v(base());
    assert!(r.issues.is_empty(), "{:#?}", r.issues);
    assert_eq!(r.to_json(), json!([]));
}

#[test]
fn r402_start_and_end() {
    let r = check_v(with_nodes(vec![node("n1", "start", json!({}))], vec![]));
    let i = only(&r, "start.multiple");
    assert_eq!((i.level, i.node.as_deref()), (Level::Error, Some("n1")));
    assert!(i.message.contains("`n0`") && i.message.contains("`n1`"));
    let mut v = base();
    v["nodes"] = json!([{ "id": "n9", "kind": "end", "kindVersion": 1 }]);
    v["edges"] = json!([]);
    assert!(check_v(v).has("start.missing"));
    let mut v = base();
    v["nodes"].as_array_mut().unwrap().remove(1);
    v["edges"] = json!([]);
    assert!(check_v(v).has("end.missing"));
}

#[test]
fn r402_edge_type_and_endpoints() {
    // list<file> cannot go into a text input; the edge is named
    let r = check_v(with_nodes(vec![node("s", "summarise", json!({}))], vec![edge("bad", "n0.files", "s.text")]));
    let i = only(&r, "edge.type");
    assert_eq!((i.edge.as_deref(), i.node.as_deref(), i.port.as_deref()), (Some("bad"), Some("s"), Some("text")));
    assert!(i.message.contains("list<file>") && i.message.contains("text"));
    // a conversion that is safe is not an error: number to text, anything to json
    let r = check_v(with_nodes(vec![node("p", "prompt", json!({})), node("s", "summarise", json!({}))], vec![edge("a", "n0.prompt", "p.in"), edge("b", "n0.files", "p.in"), edge("c", "p.usage", "s.text")]));
    assert!(r.has("input.multiple") && only(&r, "edge.type").edge.as_deref() == Some("c"), "json -> text is refused: {:#?}", r.issues);
    // missing node, missing port, a malformed endpoint
    let r = check_v(with_nodes(vec![], vec![edge("x1", "n0.prompt", "ghost.in"), edge("x2", "n0.nope", "n9.result"), edge("x3", "nodot", "n9.result")]));
    assert_eq!(only(&r, "edge.node").edge.as_deref(), Some("x1"));
    let p = only(&r, "edge.port");
    assert!(p.message.contains("nope") && p.message.contains("prompt, inputs"), "{}", p.message);
    assert_eq!(only(&r, "edge.endpoint").edge.as_deref(), Some("x3"));
}

#[test]
fn r402_required_inputs() {
    let r = check_v(with_nodes(vec![node("s", "summarise", json!({}))], vec![]));
    let i = only(&r, "input.required");
    assert_eq!((i.node.as_deref(), i.port.as_deref()), (Some("s"), Some("text")));
    assert!(i.message.contains("s.text"));
    // a default in the node's config feeds it (three places)
    for cfg in [json!({ "text": "x" }), json!({ "defaults": { "text": "x" } }), json!({ "inputs": { "text": "x" } })] {
        assert!(!check_v(with_nodes(vec![node("s", "summarise", cfg)], vec![])).has("input.required"));
    }
    assert!(check_v(with_nodes(vec![node("s", "summarise", json!({ "text": null }))], vec![])).has("input.required"));
}

#[test]
fn r402_two_edges_on_one_input() {
    let r = check_v(with_nodes(vec![node("s", "summarise", json!({}))], vec![edge("a", "n0.prompt", "s.text"), edge("b", "n0.sender", "s.text")]));
    let i = only(&r, "input.multiple");
    assert_eq!((i.node.as_deref(), i.port.as_deref(), i.edge.as_deref()), (Some("s"), Some("text"), Some("b")));
    // a `many` input (the end's result, a join's lines) takes several
    let r = check_v(with_nodes(vec![], vec![edge("a", "n0.sender", "n9.result")]));
    assert!(!r.has("input.multiple"));
    // fan-out is fine
    let r = check_v(with_nodes(vec![node("s", "summarise", json!({})), node("t", "summarise", json!({}))], vec![edge("a", "n0.prompt", "s.text"), edge("b", "n0.prompt", "t.text")]));
    assert!(!r.has("input.multiple"));
}

#[test]
fn r402_unreachable_is_a_warning() {
    let r = check_v(with_nodes(vec![node("lost", "summarise", json!({ "text": "x" })), json!({ "id": "nn", "kind": "note", "kindVersion": 1 })], vec![]));
    let i = only(&r, "node.unreachable");
    assert_eq!((i.level, i.node.as_deref()), (Level::Warn, Some("lost")));
    assert!(r.is_valid(), "a warning does not make it invalid");
    assert_eq!(r.errors().count(), 0);
}

#[test]
fn r402_kinds_and_config() {
    let r = check_v(with_nodes(vec![node("x", "acme/nothing", json!({}))], vec![edge("a", "n0.prompt", "x.in")]));
    let i = only(&r, "kind.unknown");
    assert_eq!(i.node.as_deref(), Some("x"));
    assert!(!r.has("edge.port"), "no cascade of port errors for a node whose kind is unknown");
    let mut n = node("p", "prompt", json!({}));
    n["kindVersion"] = json!(2);
    let r = check_v(with_nodes(vec![n], vec![edge("a", "n0.prompt", "p.in")]));
    assert!(only(&r, "kind.version").message.contains("version 2"));
    let r = check_v(with_nodes(vec![node("p", "prompt", json!({ "responseFormat": "xml", "maxTokens": 0 }))], vec![edge("a", "n0.prompt", "p.in")]));
    assert_eq!(r.with_code("config.invalid").len(), 2);
    // a custom kind registered by manifest becomes known, with its ports
    let mut reg = DefaultRegistry::new();
    reg.register(serde_json::from_value(json!({ "schema": 1, "kind": "acme/pr-label", "version": "1.2.0", "ports": { "in": [{ "name": "pr", "type": "json", "required": true }], "out": [{ "name": "labels", "type": "list<text>" }] } })).unwrap()).unwrap();
    let d = doc::from_value(with_nodes(vec![node("x", "acme/pr-label", json!({}))], vec![edge("a", "n0.inputs", "x.pr"), edge("b", "x.labels", "n9.result")])).unwrap();
    let r = validate(&d, &Options::new(&reg));
    assert!(r.issues.is_empty(), "{:#?}", r.issues);
}

#[test]
fn r402_missing_laws_prompts_and_models() {
    let mut p = node("p", "prompt", json!({ "system": "{{prompt:nope}}", "model": { "via": "ollama", "model": "m", "fallback": [{ "via": "ollama", "model": "m2" }] } }));
    p["laws"] = json!(["law-tone", "law-ghost"]);
    p["prompts"] = json!(["pr-ghost"]);
    let mut v = with_nodes(vec![p], vec![edge("a", "n0.prompt", "p.in")]);
    v["sections"] = json!([{ "id": "s1", "nodes": ["p", "zz"], "laws": ["grp-ghost"] }]);
    v["defaults"] = json!({ "laws": ["law-ghost2"] });
    let d = doc::from_value(v).unwrap();
    // without a scope there is nothing to check against: no errors from rules 9 and 10
    let r = check(&d);
    assert!(!r.has("law.missing") && !r.has("prompt.missing") && !r.has("model.unreachable"));
    let r = validate(&d, &Options::new(&reg()).scope(&Scope));
    let laws: Vec<&str> = r.with_code("law.missing").iter().map(|i| i.message.as_str()).collect();
    assert_eq!(laws.len(), 3, "{laws:?}");
    assert!(laws.iter().any(|m| m.contains("node `p`") && m.contains("law-ghost")) && laws.iter().any(|m| m.contains("section `s1`")) && laws.iter().any(|m| m.contains("defaults")));
    assert_eq!(r.with_code("prompt.missing").len(), 2);
    assert_eq!(r.with_code("model.unreachable").len(), 2, "the model and its fallback");
    assert!(r.with_code("model.unreachable").iter().all(|i| i.level == Level::Warn));
    assert_eq!(only(&r, "section.node").node.as_deref(), Some("zz"));
}

#[test]
fn r402_templates_conditions_and_timeouts() {
    let r = check_v(with_nodes(vec![
        node("p", "prompt", json!({ "user": "{{node.ghost.out.x}}", "system": "{{node.n0.out.nope}}", "stop": "{{oops" })),
        node("q", "prompt", json!({ "user": "{{inputs.zzz}} {{wat.x}}" })),
        node("c", "if", json!({ "condition": "a ==" })),
        node("w", "wait", json!({})),
        node("ap", "approve", json!({})),
        node("rt", "script", json!({})),
    ], vec![edge("a", "n0.prompt", "p.in"), edge("b", "n0.prompt", "q.in"), edge("c", "n0.prompt", "c.value"), edge("d", "n0.prompt", "ap.in")]));
    assert_eq!(r.with_code("template.node").len(), 1);
    assert_eq!(r.with_code("template.port").len(), 1);
    assert_eq!(r.with_code("template.syntax").len(), 1);
    assert!(r.has("condition.syntax") && r.has("wait.timeout") && r.has("approve.timeout"));
    assert!(r.with_code("template.root").iter().all(|i| i.level == Level::Warn));
    let mut v = with_nodes(vec![node("rt", "script", json!({}))], vec![edge("a", "n0.prompt", "rt.in")]);
    v["nodes"][2]["settings"] = json!({ "retry": { "backoffSec": 1 }, "onError": "explode" });
    let r = check_v(v);
    assert!(r.has("retry.max") && r.has("onError.invalid"));
    let mut v = base();
    v["defaults"] = json!({ "retry": { "max": 99 } });
    assert!(check_v(v).has("retry.max"));
    let r = check_v(with_nodes(vec![node("w", "wait", json!({ "seconds": 5 })), node("w2", "wait", json!({ "until": "friday", "timeoutSec": 60 }))], vec![edge("a", "n0.prompt", "w.in")]));
    assert!(!r.has("wait.timeout"));
    let r = check_v(with_nodes(vec![node("w", "wait", json!({ "until": "friday" }))], vec![]));
    assert!(only(&r, "wait.timeout").message.contains("`w`"));
}

#[test]
fn r402_duplicates() {
    let r = check_v(with_nodes(vec![node("n0", "note", json!({}))], vec![edge("e0", "n0.prompt", "n9.result")]));
    assert!(r.has("node.duplicate") && r.has("edge.duplicate"));
    let mut v = base();
    v["inputs"] = json!([{ "name": "a", "type": "txt" }, { "name": "a", "type": "text" }]);
    let r = check_v(v);
    assert!(r.has("port.type") && r.has("port.duplicate"));
}

// ---------------------------------------------------------------- R403

/// start -> join -> prompt -> (loop | straight back to the join) ; the join takes many lines, so the way back is legal
fn cycle_doc(through_loop: Option<Value>) -> Value {
    let mut nodes = vec![node("j", "join", json!({})), node("p", "prompt", json!({}))];
    let mut edges = vec![edge("e1", "n0.prompt", "j.lines"), edge("e2", "j.result", "p.in")];
    match through_loop {
        Some(cfg) => {
            nodes.push(node("L", "loop", cfg));
            edges.extend([edge("e3", "p.text", "L.in"), edge("e4", "L.again", "j.lines"), edge("e5", "L.done", "n9.result")]);
        }
        None => edges.extend([edge("e3", "p.text", "j.lines"), edge("e5", "p.text", "n9.result")]),
    }
    with_nodes(nodes, edges)
}

#[test]
fn r403_a_cycle_without_a_loop_is_an_error() {
    let r = check_v(cycle_doc(None));
    let i = only(&r, "graph.cycle");
    assert_eq!(i.level, Level::Error);
    assert!(i.message.contains("`j`") && i.message.contains("`p`") && i.message.contains("loop"), "{}", i.message);
    assert!(!r.is_valid());
    // a node that only hangs off the cycle is not named
    let mut v = cycle_doc(None);
    v["nodes"].as_array_mut().unwrap().push(node("tail", "summarise", json!({})));
    v["edges"].as_array_mut().unwrap().push(edge("e7", "p.text", "tail.text"));
    assert!(!only(&check_v(v), "graph.cycle").message.contains("tail"));
    // a node wired to itself
    let r = check_v(with_nodes(vec![node("j", "join", json!({}))], vec![edge("e1", "n0.prompt", "j.lines"), edge("e2", "j.result", "j.lines")]));
    assert!(only(&r, "graph.cycle").message.contains("`j`"));
}

#[test]
fn r403_the_same_cycle_through_a_loop_with_a_limit_is_valid() {
    let r = check_v(cycle_doc(Some(json!({ "mode": "until", "condition": "state.round >= 3", "maxIterations": 3 }))));
    assert!(r.issues.is_empty(), "{:#?}", r.issues);
    // a count loop's `count` is its limit
    let r = check_v(cycle_doc(Some(json!({ "mode": "count", "count": 5 }))));
    assert!(r.issues.is_empty(), "{:#?}", r.issues);
}

#[test]
fn r403_a_loop_with_no_limit_is_an_error() {
    let r = check_v(cycle_doc(Some(json!({ "mode": "until", "condition": "true" }))));
    let i = only(&r, "loop.no-limit");
    assert_eq!((i.level, i.node.as_deref()), (Level::Error, Some("L")));
    assert!(!r.has("graph.cycle"), "the loop is there: only its limit is missing");
    for bad in [json!(0), json!(101), json!(2.5), json!("ten")] {
        let r = check_v(cycle_doc(Some(json!({ "mode": "count", "maxIterations": bad }))));
        assert!(r.has("loop.limit"), "{bad}");
    }
    // while / until need a condition that parses
    assert!(check_v(cycle_doc(Some(json!({ "maxIterations": 3 })))).has("loop.condition"));
    assert!(check_v(cycle_doc(Some(json!({ "maxIterations": 3, "condition": "((" })))).has("condition.syntax"));
}

// ---------------------------------------------------------------- R404

fn io(id: &str, kind: &str, name: &str, ty: &str) -> Value { node(id, kind, json!({ "name": name, "type": ty })) }

/// a sub-graph `key` with input x (text) and output y (text); `inner` is an extra node + edges inside
fn sg(key: &str, inner: Option<(&str, &str)>) -> (String, Value) {
    let mut nodes = vec![io("i", "input", "x", "text"), io("o", "output", "y", "text")];
    let mut edges = vec![];
    match inner {
        None => edges.push(edge("w", "i.value", "o.value")),
        Some((id, r)) => {
            nodes.push(node(id, "subgraph", json!({ "ref": r })));
            edges.extend([edge("w1", &format!("i.value"), &format!("{id}.x")), edge("w2", &format!("{id}.y"), "o.value")]);
        }
    }
    (key.to_string(), json!({ "nodes": nodes, "edges": edges, "inputs": [{ "name": "x", "type": "text" }], "outputs": [{ "name": "y", "type": "text" }] }))
}

fn with_subgraphs(list: Vec<(String, Value)>, top_ref: &str) -> Value {
    let mut v = with_nodes(vec![node("s", "subgraph", json!({ "ref": top_ref }))], vec![edge("a", "n0.prompt", "s.x"), edge("b", "s.y", "n9.result")]);
    v["subgraphs"] = Value::Object(list.into_iter().collect());
    v
}

fn chain(depth: usize) -> Value {
    let list = (1..=depth).map(|k| if k < depth { sg(&format!("g{k}"), Some(("s", &format!("g{}", k + 1)))) } else { sg(&format!("g{k}"), None) }).collect();
    with_subgraphs(list, "g1")
}

#[test]
fn r404_matching_ports_are_valid() {
    let r = check_v(with_subgraphs(vec![sg("g", None)], "g"));
    assert!(r.issues.is_empty(), "{:#?}", r.issues);
    // the box's ports are the inside's: a wire to a port it does not have is refused, a wrong type too
    let mut v = with_subgraphs(vec![sg("g", None)], "g");
    v["edges"].as_array_mut().unwrap().push(edge("c", "n0.prompt", "s.nope"));
    assert!(check_v(v).has("edge.port"));
    let mut v = with_subgraphs(vec![sg("g", None)], "g");
    v["edges"][1]["from"] = json!("n0.files");
    v["edges"][1]["id"] = json!("a");
    v["edges"].as_array_mut().unwrap().remove(0);
    assert_eq!(only(&check_v(v), "edge.type").edge.as_deref(), Some("a"));
    // an unfed sub-graph input is a required input of the box
    let mut v = with_subgraphs(vec![sg("g", None)], "g");
    v["edges"].as_array_mut().unwrap().retain(|e| e["id"] != "a");
    let i = only(&check_v(v), "input.required");
    assert_eq!((i.node.as_deref(), i.port.as_deref()), (Some("s"), Some("x")));
    // ports come from the nodes inside when the sub-graph declares none
    let (k, mut g) = sg("g", None);
    g.as_object_mut().unwrap().remove("inputs");
    g.as_object_mut().unwrap().remove("outputs");
    assert!(check_v(with_subgraphs(vec![(k, g)], "g")).issues.is_empty());
}

#[test]
fn r404_mismatched_ports_are_errors() {
    // a declared input with no input node, and an input node nobody declared
    let (k, mut g) = sg("g", None);
    g["inputs"] = json!([{ "name": "q", "type": "text" }]);
    let r = check_v(with_subgraphs(vec![(k, g)], "g"));
    let msgs: Vec<String> = r.with_code("subgraph.ports").iter().map(|i| i.message.clone()).collect();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("declares the input `q`")) && msgs.iter().any(|m| m.contains("does not declare an input `x`")));
    assert!(r.with_code("subgraph.ports").iter().all(|i| i.graph.as_deref() == Some("g")));
    // the same name, a different type
    let (k, mut g) = sg("g", None);
    g["outputs"] = json!([{ "name": "y", "type": "number" }]);
    let m = only(&check_v(with_subgraphs(vec![(k, g)], "g")), "subgraph.ports").message;
    assert!(m.contains("declared number") && m.contains("text"), "{m}");
    // a reference to a sub-graph that is not there
    assert!(check_v(with_subgraphs(vec![sg("g", None)], "nope")).has("subgraph.missing"));
    // start inside a sub-graph, input at the top, an unused sub-graph (a warning)
    let (k, mut g) = sg("g", None);
    g["nodes"].as_array_mut().unwrap().push(node("st", "start", json!({})));
    assert!(check_v(with_subgraphs(vec![(k, g)], "g")).has("start.in-subgraph"));
    assert!(check_v(with_nodes(vec![io("i", "input", "x", "text")], vec![])).has("node.misplaced"));
    let r = check_v(with_subgraphs(vec![sg("g", None), sg("spare", None)], "g"));
    assert_eq!(only(&r, "subgraph.unused").level, Level::Warn);
    assert!(r.is_valid());
}

#[test]
fn r404_a_subgraph_containing_itself_is_an_error() {
    let r = check_v(with_subgraphs(vec![sg("g", Some(("again", "g")))], "g"));
    let i = only(&r, "subgraph.recursive");
    assert!(i.message.contains("g -> g") && i.message.contains("contains itself"), "{}", i.message);
    assert_eq!(i.graph.as_deref(), Some("g"));
    // through another one
    let r = check_v(with_subgraphs(vec![sg("a", Some(("x1", "b"))), sg("b", Some(("x2", "a")))], "a"));
    assert_eq!(r.with_code("subgraph.recursive").len(), 1, "{:#?}", r.issues);
    assert!(r.with_code("subgraph.recursive")[0].message.contains("a -> b -> a"));
    assert!(!r.has("subgraph.depth"), "a recursion is reported once, not as an endless depth");
}

#[test]
fn r404_depth_nine_is_refused() {
    for ok in [1, 2, 8] {
        let r = check_v(chain(ok));
        assert!(r.issues.is_empty(), "depth {ok}: {:#?}", r.issues);
    }
    let r = check_v(chain(9));
    let i = only(&r, "subgraph.depth");
    assert!(i.message.contains("9 levels") && i.message.contains("at most 8") && i.node.as_deref() == Some("s"), "{}", i.message);
    assert!(!r.is_valid());
    // the limit is an option
    let d = doc::from_value(chain(3)).unwrap();
    let r = validate(&d, &Options { max_depth: 2, ..Options::new(&reg()) });
    assert!(r.has("subgraph.depth"));
}

// ---------------------------------------------------------------- R405

#[test]
fn r405_diff_names_what_changed() {
    let v1 = doc::parse(FIXTURE).unwrap();
    assert!(diff(&v1, &v1).is_empty());
    let mut v2 = v1.clone();
    // a removed node (and its edge), an added node (and its edge), a changed config key, a rewired edge
    v2.nodes.retain(|n| n.id != "n7");
    v2.edges.retain(|e| e.id != "e9");
    v2.nodes.push(Node::new("n10", "summarise").with_config(json!({ "text": "x" })));
    v2.edges.push(Edge::new("e10", "n0.prompt", "n10.text"));
    let n3 = v2.nodes.iter_mut().find(|n| n.id == "n3").unwrap();
    n3.config.as_mut().unwrap()["responseFormat"] = json!("json");
    n3.config.as_mut().unwrap()["temperature"] = json!(0.2);
    n3.config.as_mut().unwrap().as_object_mut().unwrap().remove("system");
    n3.label = Some("Draft the notes".into());
    n3.pos = Some(vec![1.into(), 2.into()]);
    v2.edges.iter_mut().find(|e| e.id == "e6").unwrap().to = "n8.in".into();
    v2.title = Some("Notes".into());
    v2.version = Some(5);
    v2.message = Some("changed".into());
    let d = diff(&v1, &v2);
    assert_eq!(d.graph.added_nodes, ["n10"]);
    assert_eq!(d.graph.removed_nodes, ["n7"]);
    assert_eq!(d.graph.added_edges, ["e10"]);
    assert_eq!(d.graph.removed_edges, ["e9"]);
    assert_eq!(d.graph.rewired_edges, vec![diff::Rewire { edge: "e6".into(), before: ("n4.no".into(), "n8.back".into()), after: ("n4.no".into(), "n8.in".into()) }]);
    assert_eq!(d.graph.changed_nodes.len(), 1);
    let c = &d.graph.changed_nodes[0];
    assert_eq!(c.id, "n3");
    let keys: Vec<&str> = c.config.iter().map(|k| k.key.as_str()).collect();
    assert_eq!(keys, ["responseFormat", "system", "temperature"]);
    assert_eq!((c.config[0].before.clone(), c.config[0].after.clone()), (Some(json!("text")), Some(json!("json"))));
    assert!(c.config[1].after.is_none() && c.config[2].before.is_none());
    assert_eq!(c.fields, ["label"], "moving a node (pos) is not a change");
    assert_eq!(d.meta, ["title"], "version / message are the publish stamp, not content");
    let lines = d.summary();
    for want in ["added node n10", "removed node n7", "node n3 changed config `responseFormat`", "rewired edge e6: n4.no -> n8.back became n4.no -> n8.in", "added edge e10", "removed edge e9"] {
        assert!(lines.iter().any(|l| l == want), "{want} in {lines:?}");
    }
    assert_eq!(d.to_json()["added_nodes"], json!(["n10"]));
    // a changed kind, and sub-graphs
    let mut v3 = v1.clone();
    v3.nodes.iter_mut().find(|n| n.id == "n2").unwrap().kind = "prompt".into();
    assert_eq!(diff(&v1, &v3).graph.changed_nodes[0].kind, Some(("summarise".into(), "prompt".into())));
    let a = doc::from_value(with_subgraphs(vec![sg("g", None), sg("old", None)], "g")).unwrap();
    let mut b = a.clone();
    b.subgraphs.as_mut().unwrap().remove("old");
    let fresh: Subgraph = serde_json::from_value(sg("new", None).1).unwrap();
    b.subgraphs.as_mut().unwrap().insert("new".into(), fresh);
    b.subgraphs.as_mut().unwrap().get_mut("g").unwrap().nodes.push(Node::new("extra", "note"));
    let d = diff(&a, &b);
    assert_eq!((d.added_subgraphs, d.removed_subgraphs), (vec!["new".to_string()], vec!["old".to_string()]));
    assert_eq!(d.subgraphs["g"].added_nodes, ["extra"]);
}

// ---------------------------------------------------------------- R406

fn temp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("pgm-workflow-{tag}-{}", pgm_core_internal::crypto::random_hex(6)));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn r406_a_bad_file_is_moved_aside_and_listing_goes_on() {
    let dir = temp("store");
    std::fs::write(dir.join("a-good.json"), FIXTURE).unwrap();
    std::fs::write(dir.join("b-broken.json"), "{ not json at all").unwrap();
    std::fs::write(dir.join("c-array.json"), "[1, 2]").unwrap();
    std::fs::write(dir.join("d-newer.json"), json!({ "schema": 99, "id": "wf-n", "name": "n", "nodes": [], "edges": [] }).to_string()).unwrap();
    std::fs::write(dir.join("e-empty.json"), "  \n").unwrap();
    std::fs::write(dir.join("f-old.json"), json!({ "id": "wf-o", "name": "o", "nodes": [], "connections": [] }).to_string()).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a workflow").unwrap();
    std::fs::write(dir.join(".hidden.json"), "{").unwrap();
    let got = store::load_dir(&dir);
    assert_eq!(got.docs.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>(), ["a-good.json", "f-old.json"]);
    assert_eq!(got.docs[1].1.schema, SCHEMA, "an older file is migrated in memory");
    assert_eq!(std::fs::read_to_string(dir.join("f-old.json")).unwrap().contains("connections"), true, "and not rewritten");
    assert_eq!(got.moved_aside.len(), 2);
    for a in &got.moved_aside {
        let name = a.to.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.contains(".json.bad-") && !a.from.exists() && a.to.exists(), "{name}");
        let stamp = name.split(".bad-").nth(1).unwrap();
        assert!(stamp.len() >= 15 && stamp.as_bytes()[8] == b'-' && stamp[..8].chars().all(|c| c.is_ascii_digit()), "a yyyymmdd-hhmmss stamp: {stamp}");
        assert!(!a.reason.is_empty());
    }
    assert_eq!(std::fs::read_to_string(dir.join("b-broken.json").with_file_name(got.moved_aside[0].to.file_name().unwrap())).unwrap(), "{ not json at all", "kept as it was");
    assert_eq!(got.newer.len(), 1);
    assert!(got.newer[0].0.exists() && got.newer[0].1.contains("newer"), "a newer file stays where it is");
    assert!(dir.join("notes.txt").exists() && dir.join("e-empty.json").exists() && dir.join(".hidden.json").exists());
    let notices = pgm_core_internal::notices::drain();
    assert!(notices.iter().any(|n| n.contains("b-broken.json")) && notices.iter().any(|n| n.contains("c-array.json")), "{notices:?}");
    // listing again: the same good files, nothing more to move
    let again = store::load_dir(&dir);
    assert_eq!((again.docs.len(), again.moved_aside.len(), again.newer.len()), (2, 0, 1));
    // a second bad file of the same name in the same second does not overwrite the first
    std::fs::write(dir.join("b-broken.json"), "{ again").unwrap();
    let third = store::load_dir(&dir);
    assert_eq!(third.moved_aside.len(), 1);
    assert_eq!(std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("b-broken.json.bad-")).count(), 2);
    // a folder that is not there is an empty list
    assert!(store::load_dir(&dir.join("nope")).docs.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r406_save_writes_the_canonical_file() {
    let dir = temp("save");
    let f = dir.join("sub").join("wf.json");
    let d = doc::parse(FIXTURE).unwrap();
    store::save(&f, &d).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), FIXTURE);
    assert_eq!(store::load_file(&f).unwrap(), d);
    assert!(!f.with_extension("json.tmp").exists());
    assert_eq!(store::load_dir(&dir.join("sub")).docs.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
