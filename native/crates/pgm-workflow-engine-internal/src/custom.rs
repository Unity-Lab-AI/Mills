//! Custom node kinds (T-203; spec 11): kinds a person makes, shared with the team. A kind is a **manifest**
//! (pgm-workflow-internal's `KindManifest`: name, semver, ports, config schema, permissions, tests) with an `impl`:
//! - `graph`: a packaged box, `impl.graph` a sub-graph (`{nodes, edges, inputs, outputs}`); runs like a `subgraph` node.
//!   "New node from selection" ([`from_selection`]) makes one.
//! - `http`: a custom call (`impl.http`, as the `tool` kind's), only to the hosts `permissions.network` declares.
//! - `script`: the transform language (`impl.code`, `impl.outputs`).
//! - `program` / `wasm` (a sandboxed program): not on any runner yet (open question 6): refused when installed.
//!
//! A manifest's **hash** (sha256 of its canonical JSON) is taken when it is published; a runner that fetches a kind
//! checks it ([`Engine::install`] with the expected hash) and refuses one that does not match.
use crate::engine::{Engine, RunInput, RunOptions};
use crate::fake::FakeHost;
use crate::host::{BoxFut, NodeCall, NodeError, NodeKind, NodeOutput, WorkflowHost};
use crate::kinds::{script::Script, tools};
use pgm_workflow_internal::kinds::KindRegistry;
use pgm_workflow_internal::types::split_endpoint;
use pgm_workflow_internal::{doc, Document, Edge, KindManifest, Node};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The manifest's hash: sha256 of its canonical JSON (keys in their declared order, unknown fields last).
pub fn hash(m: &KindManifest) -> String { pgm_core_internal::crypto::sha256_hex(serde_json::to_string(m).unwrap_or_default().as_bytes()) }

/// The implementation type of a manifest (`graph`, `http`, `script`, `program`, ...).
pub fn impl_type(m: &KindManifest) -> &str { m.implementation.as_ref().and_then(|i| i.get("type")).and_then(Value::as_str).unwrap_or("") }

/// Needs two reviewers (spec 11.4): a sandboxed program.
pub fn needs_two(m: &KindManifest) -> bool { matches!(impl_type(m), "program" | "wasm") }

/// The node's config with the kind's own definition under it (the node's settings win where both say something).
fn merged(call: &NodeCall, base: Value) -> NodeCall {
    let mut cfg = base;
    if let (Some(o), Some(Value::Object(n))) = (cfg.as_object_mut(), call.node.config.clone()) { for (k, v) in n { o.insert(k, v); } }
    let mut c = call.clone();
    c.node.config = Some(cfg);
    c
}

/// An `http` kind: its call, only to the hosts it declared (`permissions.network`; none declared: no network).
pub struct CustomHttp { pub manifest: KindManifest }
impl NodeKind for CustomHttp {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        Box::pin(async move {
            let imp = self.manifest.implementation.clone().unwrap_or(Value::Null);
            let allowed: Vec<String> = self.manifest.permissions.as_ref().and_then(|p| p.get("network")).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            let c = merged(&call, json!({ "http": imp.get("http").cloned().unwrap_or(Value::Null) }));
            tools::http_call(&c, host, &imp["http"], Some(&allowed)).await
        })
    }
    fn idempotent(&self, _node: &Node) -> bool { self.manifest.idempotent.unwrap_or(false) }
}

/// A `script` kind: the transform language, its code from the manifest.
pub struct CustomScript { pub manifest: KindManifest }
impl NodeKind for CustomScript {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        Box::pin(async move {
            let imp = self.manifest.implementation.clone().unwrap_or(Value::Null);
            let outs: Vec<Value> = self.manifest.ports.outputs.iter().filter(|p| p.ty != "error").map(|p| json!(p.name)).collect();
            let c = merged(&call, json!({ "lang": "transform", "code": imp["code"], "outputs": imp.get("outputs").cloned().unwrap_or(json!(outs)) }));
            Script.run(c, host).await
        })
    }
}

/// The stand-in a `graph` kind is registered with; the engine runs its graph itself (as a sub-graph).
pub struct GraphKind;
impl NodeKind for GraphKind {
    fn run<'a>(&'a self, call: NodeCall, _host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        Box::pin(async move { Err(NodeError::fatal("engine.bug", format!("the graph kind `{}` was not run as a graph", call.node.kind))) })
    }
}

impl Engine {
    /// Installs a custom kind (a runner fetching one from its scope's store). With `expected_hash`, a manifest whose
    /// hash differs is refused (`engine.kind-hash`); a sandboxed program is refused on this runner
    /// (`engine.kind-program`); a graph kind's graph must be one.
    pub fn install(&mut self, m: KindManifest, expected_hash: Option<&str>) -> Result<(), Vec<String>> {
        if let Some(h) = expected_hash { if hash(&m) != h { return Err(vec![format!("engine.kind-hash: `{}` {} is not the version that was published (its hash differs)", m.kind, m.version)]); } }
        match impl_type(&m) {
            "graph" => {
                let g = m.implementation.as_ref().and_then(|i| i.get("graph")).cloned().filter(Value::is_object).ok_or_else(|| vec![format!("`{}`: a graph kind needs impl.graph", m.kind)])?;
                serde_json::from_value::<pgm_workflow_internal::Subgraph>(g.clone()).map_err(|e| vec![format!("`{}`: impl.graph is not a sub-graph: {e}", m.kind)])?;
                let kind = m.kind.clone();
                self.register_custom(m, Arc::new(GraphKind))?;
                self.set_graph(&kind, g);
                Ok(())
            }
            "http" => { let k = Arc::new(CustomHttp { manifest: m.clone() }); self.register_custom(m, k) }
            "script" => { let k = Arc::new(CustomScript { manifest: m.clone() }); self.register_custom(m, k) }
            "program" | "wasm" => Err(vec![format!("engine.kind-program: `{}` is a sandboxed program, which no runner here can run yet", m.kind)]),
            t => Err(vec![format!("`{}`: impl.type `{t}` is not graph, http, script or program", m.kind)]),
        }
    }
}

/// A wire across the selection's edge, by its port name: (node, port, type).
type Crossing = BTreeMap<String, (String, String, String)>;

/// "New node from selection" (spec 11.1, R431): the selected top-level nodes become a `graph` kind. Wires inside the
/// selection stay inside; every input fed from outside becomes one of its inputs, every output read outside one of its
/// outputs (named `<node>_<port>`). Gives the manifest (version 1.0.0) and the document with the selection replaced by
/// one node of the new kind, wired as before.
pub fn from_selection(d: &Document, reg: &dyn KindRegistry, ids: &[String], kind: &str, title: &str) -> Result<(KindManifest, Document), String> {
    let sel: Vec<&Node> = d.nodes.iter().filter(|n| ids.contains(&n.id)).collect();
    if sel.is_empty() || sel.len() != ids.len() { return Err("select one or more nodes of the workflow".into()); }
    if sel.iter().any(|n| matches!(n.kind.as_str(), "start" | "end")) { return Err("start and end stay in the workflow: select the nodes between them".into()); }
    let inside = |id: &str| ids.iter().any(|x| x == id);
    let ends = |e: &Edge| -> Option<(String, String, String, String)> { let (a, ap) = split_endpoint(&e.from)?; let (b, bp) = split_endpoint(&e.to)?; Some((a.into(), ap.into(), b.into(), bp.into())) };
    let port_ty = |node: &str, port: &str, out: bool| -> String {
        d.node(node).and_then(|n| reg.ports(n)).and_then(|p| if out { p.output(port).map(|x| x.ty.to_string()) } else { p.input(port).map(|x| x.ty.to_string()) }).unwrap_or_else(|| "json".into())
    };
    let (mut ins, mut outs): (Crossing, Crossing) = (BTreeMap::new(), BTreeMap::new());
    let (mut inner, mut new_edges) = (vec![], vec![]);
    let box_id = { let mut i = 1; while d.node(&format!("k{i}")).is_some() { i += 1; } format!("k{i}") };
    for e in &d.edges {
        let Some((a, ap, b, bp)) = ends(e) else { continue };
        match (inside(&a), inside(&b)) {
            (true, true) => inner.push(e.clone()),
            (false, true) => {
                let name = format!("{b}_{bp}");
                ins.insert(name.clone(), (b.clone(), bp.clone(), port_ty(&b, &bp, false)));
                new_edges.push(Edge::new(&e.id, &e.from, &format!("{box_id}.{name}")));
            }
            (true, false) => {
                let name = format!("{a}_{ap}");
                outs.insert(name.clone(), (a.clone(), ap.clone(), port_ty(&a, &ap, true)));
                new_edges.push(Edge::new(&e.id, &format!("{box_id}.{name}"), &e.to));
            }
            (false, false) => new_edges.push(e.clone()),
        }
    }
    let mut nodes: Vec<Value> = sel.iter().map(|n| serde_json::to_value(n).unwrap_or(Value::Null)).collect();
    let mut edges: Vec<Value> = inner.iter().map(|e| serde_json::to_value(e).unwrap_or(Value::Null)).collect();
    for (name, (b, bp, ty)) in &ins {
        nodes.push(json!({ "id": format!("in_{name}"), "kind": "input", "config": { "name": name, "type": ty } }));
        edges.push(json!({ "id": format!("ei_{name}"), "from": format!("in_{name}.value"), "to": format!("{b}.{bp}") }));
    }
    for (name, (a, ap, ty)) in &outs {
        nodes.push(json!({ "id": format!("out_{name}"), "kind": "output", "config": { "name": name, "type": ty } }));
        edges.push(json!({ "id": format!("eo_{name}"), "from": format!("{a}.{ap}"), "to": format!("out_{name}.value") }));
    }
    let decl = |m: &Crossing| -> Vec<Value> { m.iter().map(|(n, (_, _, t))| json!({ "name": n, "type": t })).collect() };
    let graph = json!({ "nodes": nodes, "edges": edges, "inputs": decl(&ins), "outputs": decl(&outs) });
    let mut port_out = decl(&outs);
    port_out.push(json!({ "name": "error", "type": "error" }));
    let manifest: KindManifest = serde_json::from_value(json!({
        "schema": 1, "kind": kind, "version": "1.0.0", "title": title, "category": "Custom",
        "ports": { "in": decl(&ins), "out": port_out }, "config": { "type": "object" },
        "impl": { "type": "graph", "graph": graph }, "idempotent": true,
    })).map_err(|e| e.to_string())?;
    let problems = manifest.problems();
    if !problems.is_empty() { return Err(problems.join("; ")); }
    let mut nd = d.clone();
    let pos = sel[0].pos.clone();
    nd.nodes.retain(|n| !inside(&n.id));
    let mut k = Node::new(&box_id, kind);
    k.label = Some(title.to_string());
    k.pos = pos;
    nd.nodes.push(k);
    nd.edges = new_edges;
    Ok((manifest, nd))
}

/// Runs a manifest's `tests` (spec 11.3: on publish) in a throw-away engine with the fake host: each test
/// `{name, in: {port: value}, config?, expect: {port: value}, http?: {status, body}}` feeds the inputs, runs the node
/// and compares the outputs it names. Gives the failures (empty: all passed).
pub async fn run_tests(m: &KindManifest, others: &[KindManifest]) -> Vec<String> {
    let mut failures = vec![];
    for (i, t) in m.tests.iter().flatten().enumerate() {
        let name = t.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("test {}", i + 1));
        let mut e = Engine::with_builtins();
        for o in others { let _ = e.install(o.clone(), None); }
        if let Err(p) = e.install(m.clone(), None) { failures.push(format!("{name}: {}", p.join("; "))); continue; }
        let host = FakeHost::new();
        if let Some(h) = t.get("http") { host.http_reply(h["status"].as_u64().unwrap_or(200) as u16, &h.get("body").map(|b| if let Value::String(s) = b { s.clone() } else { b.to_string() }).unwrap_or_default()); }
        let ins: Vec<String> = m.ports.inputs.iter().map(|p| p.name.clone()).collect();
        let outs: Vec<&str> = m.ports.outputs.iter().filter(|p| p.ty != "error").map(|p| p.name.as_str()).collect();
        // start -> a transform that hands each test input to its port -> the node -> end
        let code: String = ins.iter().map(|p| format!("{p} = input.{p}\n")).collect::<String>() + "done = true\n";
        let typed: Vec<Value> = m.ports.inputs.iter().map(|p| json!({ "name": p.name, "type": p.ty })).chain(std::iter::once(json!({ "name": "done", "type": "json" }))).collect();
        let nodes = vec![json!({ "id": "s", "kind": "start" }), json!({ "id": "x", "kind": "script", "config": { "code": code, "outputs": typed } }),
            json!({ "id": "k", "kind": m.kind, "kindVersion": m.major(), "config": t.get("config").cloned().unwrap_or(json!({})) }), json!({ "id": "e", "kind": "end" })];
        let mut edges = vec![json!({ "id": "t0", "from": "s.inputs", "to": "x.in" })];
        for (j, p) in ins.iter().enumerate() { edges.push(json!({ "id": format!("t{}", j + 1), "from": format!("x.{p}"), "to": format!("k.{p}") })); }
        let tail = match outs.first() { Some(o) => format!("k.{o}"), None => "x.done".to_string() };
        edges.push(json!({ "id": "tz", "from": tail, "to": "e.result" }));
        let dv = json!({ "schema": 1, "id": "wf-test-kind", "name": "test-kind", "nodes": nodes, "edges": edges });
        let Ok(d) = doc::from_value(dv) else { failures.push(format!("{name}: could not build the test")); continue };
        let mut input = RunInput::prompt("");
        input.inputs = t.get("in").and_then(Value::as_object).cloned().unwrap_or_default();
        let h: Arc<dyn WorkflowHost> = host.clone();
        let r = e.start(Arc::new(d), input, h, RunOptions::default()).await;
        let got: Map<String, Value> = r.log.iter().rev().find(|ev| ev["type"] == "node_end" && ev["key"] == "k").and_then(|ev| ev["outputs"].as_object().cloned()).unwrap_or_default();
        for (k, want) in t.get("expect").and_then(Value::as_object).cloned().unwrap_or_default() {
            if got.get(&k) != Some(&want) { failures.push(format!("{name}: `{k}` was {} not {want}{}", got.get(&k).map(|v| v.to_string()).unwrap_or_else(|| "missing".into()), r.error.as_ref().map(|e| format!(" ({})", e["message"].as_str().unwrap_or(""))).unwrap_or_default())); }
        }
    }
    failures
}
