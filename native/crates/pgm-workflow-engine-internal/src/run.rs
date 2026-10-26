//! One run: tokens on wires (spec 4).
//!
//! Every wire carries one **token** per instance of its scope: a value, *skip* (the way was not taken: a switch's other
//! ports, a skipped node's outputs) or a *failure* (an error nobody caught yet). A node fires when every wire into it has
//! its token: with a value on each required input it runs; when nothing but skips came it is skipped and skips its
//! outputs; a failure passes straight through until a join, a loop, a sub-graph's output or an end takes it (an end:
//! the run fails). An `end` that gets a value ends the run.
//!
//! A **scope** is one instance of a region (plan.rs): the graph itself, a loop round, a parallel line, a sub-graph
//! call. Its nodes see the values of the scopes around it; the region's owner (the loop, the parallel, the sub-graph
//! node) starts the instances and hands their results out.
use crate::cancel::Cancel;
use crate::host::{err_value, walk, Ask, NodeCall, NodeError, NodeKind, WorkflowHost};
use crate::log::{Prior, RunLog};
use crate::plan::{Plan, RKind};
use futures_util::future::{BoxFuture, FutureExt};
use futures_util::stream::{FuturesUnordered, StreamExt};
use pgm_workflow_internal::{expr, Document, Node};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Semaphore;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Val(Value),
    Skip,
    Err(Value),
    /// What a set of parallel lines hands its join: one token per line, in line order.
    Lines(Vec<Tok>),
}

#[derive(Debug, Clone)]
pub(crate) enum Stop {
    End { name: String, result: Value },
    Fail(Value),
    Limit { name: String, value: Value },
    GaveUp(Value),
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Mode {
    Fresh,
    Resume,
    Replay,
}

#[derive(Debug, Clone)]
pub(crate) struct Limits {
    pub max_seconds: Option<f64>,
    pub max_nodes: u64,
    pub max_cost: Option<f64>,
    pub max_parallel: usize,
}

pub(crate) struct Run {
    pub id: String,
    pub doc: Arc<Document>,
    pub kinds: Arc<HashMap<String, Arc<dyn NodeKind>>>,
    pub host: Arc<dyn WorkflowHost>,
    pub plans: HashMap<Option<String>, Arc<Plan>>,
    pub log: RunLog,
    pub prior: Prior,
    pub mode: Mode,
    pub cancel: Cancel,
    pub stop: Mutex<Option<Stop>>,
    pub limits: Limits,
    pub sem: Arc<Semaphore>,
    pub node_runs: AtomicU64,
    pub cost: Mutex<f64>,
    pub state: Mutex<Map<String, Value>>,
    /// `{prompt, inputs, files, sender, project}`: what came in.
    pub input: Value,
    pub waiting: Mutex<Vec<Value>>,
    /// Instances this drive has logged a `node_start` for.
    pub started: Mutex<std::collections::HashSet<String>>,
    /// The engine running this run (a `sub-workflow` starts its child with it).
    pub engine: crate::Engine,
    /// How many `sub-workflow` runs this one is inside.
    pub depth: usize,
    /// The laws of the run's scope (read from the host once, when the drive starts).
    pub laws: crate::host::LawBook,
}

impl Run {
    pub fn stopped(&self) -> bool { lock(&self.stop).is_some() }

    /// The first reason to stop wins; everything still running is cancelled.
    pub fn halt(&self, s: Stop) {
        {
            let mut g = lock(&self.stop);
            if g.is_some() { return; }
            *g = Some(s);
        }
        self.cancel.cancel();
    }

    pub fn limit(&self, name: &str, value: Value, node: Option<&str>) {
        if self.stopped() { return; }
        self.log.emit("limit", json!({ "limit": name, "node": node, "value": value }));
        self.halt(Stop::Limit { name: name.to_string(), value });
    }

    fn count_run(&self) -> bool {
        let n = self.node_runs.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.limits.max_nodes { self.limit("maxNodes", json!(self.limits.max_nodes), None); return false; }
        true
    }

    fn add_usage(&self, u: &Value) {
        let c = u.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        if c == 0.0 { return; }
        let total = { let mut g = lock(&self.cost); *g += c; *g };
        if let Some(max) = self.limits.max_cost { if total > max { self.limit("maxCost", json!(max), None); } }
    }

    pub fn cost(&self) -> f64 { *lock(&self.cost) }
}

pub(crate) struct Scope {
    pub plan: Arc<Plan>,
    pub region: Option<usize>,
    pub owner: Option<usize>,
    pub path: String,
    pub parent: Option<Arc<Scope>>,
    pub entry: HashMap<String, Tok>,
    pub sub_in: Map<String, Value>,
    pub sub_out: Mutex<HashMap<String, Tok>>,
    pub extra: Map<String, Value>,
    pub depth: usize,
    pub cancel: Cancel,
    pub iteration: Option<u64>,
    pub line: Option<String>,
    pub outs: Mutex<HashMap<usize, HashMap<String, Tok>>>,
    pub escapes: Mutex<HashMap<usize, Tok>>,
}

impl Scope {
    /// The top of a graph: the document itself, or one call of a sub-graph.
    pub fn graph(plan: Arc<Plan>, path: String, depth: usize, cancel: Cancel, sub_in: Map<String, Value>, extra: Map<String, Value>) -> Arc<Scope> {
        Arc::new(Scope {
            plan, region: None, owner: None, path, parent: None, entry: HashMap::new(), sub_in, sub_out: Mutex::new(HashMap::new()), extra, depth, cancel,
            iteration: None, line: None, outs: Mutex::new(HashMap::new()), escapes: Mutex::new(HashMap::new()),
        })
    }

    /// One instance of a region (a loop round, a parallel line) inside `parent`.
    #[allow(clippy::too_many_arguments)]
    fn region(parent: &Arc<Scope>, region: usize, seg: String, entry: HashMap<String, Tok>, extra: Map<String, Value>, iteration: Option<u64>, line: Option<String>, cancel: Cancel) -> Arc<Scope> {
        Arc::new(Scope {
            plan: parent.plan.clone(), region: Some(region), owner: Some(parent.plan.regions[region].owner), path: parent.child_path(&seg), parent: Some(parent.clone()),
            entry, sub_in: Map::new(), sub_out: Mutex::new(HashMap::new()), extra, depth: parent.depth, cancel,
            iteration: iteration.or(parent.iteration), line: line.or_else(|| parent.line.clone()), outs: Mutex::new(HashMap::new()), escapes: Mutex::new(HashMap::new()),
        })
    }

    pub fn key(&self, id: &str) -> String { if self.path.is_empty() { id.to_string() } else { format!("{id}@{}", self.path) } }
    pub fn child_path(&self, seg: &str) -> String { if self.path.is_empty() { seg.to_string() } else { format!("{}/{seg}", self.path) } }

    /// The token on edge `e` as this scope sees it; None while it has not arrived.
    pub fn resolve(&self, e: usize) -> Option<Tok> {
        let edge = &self.plan.edges[e];
        if Some(edge.src) == self.owner { return Some(self.entry.get(&edge.sport).cloned().unwrap_or(Tok::Skip)); }
        if !self.plan.within(edge.src, self.region) {
            return match &self.parent { Some(p) => p.resolve(e), None => Some(Tok::Skip) };
        }
        match self.plan.child_region_containing(self.region, edge.src) {
            None => lock(&self.outs).get(&edge.src).map(|m| m.get(&edge.sport).cloned().unwrap_or(Tok::Skip)),
            Some(c) => {
                let owner = self.plan.regions[c].owner;
                if lock(&self.outs).contains_key(&owner) { Some(lock(&self.escapes).get(&e).cloned().unwrap_or(Tok::Skip)) } else { None }
            }
        }
    }

    /// What templates and conditions read.
    fn vars(&self, run: &Run) -> Value {
        let mut node = Map::new();
        let mut extra = Map::new();
        let mut cur: Option<&Scope> = Some(self);
        while let Some(s) = cur {
            for (k, v) in &s.extra { if !extra.contains_key(k) { extra.insert(k.clone(), v.clone()); } }
            for (n, outs) in lock(&s.outs).iter() {
                let id = &s.plan.nodes[*n].id;
                if node.contains_key(id) { continue; }
                let o: Map<String, Value> = outs.iter().filter_map(|(p, t)| if let Tok::Val(v) = t { Some((p.clone(), v.clone())) } else { None }).collect();
                node.insert(id.clone(), json!({ "out": o }));
            }
            cur = s.parent.as_deref();
        }
        let mut v = run.input.as_object().cloned().unwrap_or_default();
        v.insert("node".into(), Value::Object(node));
        v.insert("state".into(), Value::Object(lock(&run.state).clone()));
        for (k, x) in extra { v.insert(k, x); }
        Value::Object(v)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Inputs {
    pub values: Map<String, Value>,
    /// A join's lines in line order (None: not there yet).
    pub lines: Vec<Option<Tok>>,
    /// A failure that reached an `end` or a sub-graph's `output`.
    pub err: Option<Value>,
}

enum Decision {
    NotYet,
    Resolve(HashMap<String, Tok>, Option<&'static str>),
    Fire(Inputs),
}

pub(crate) enum Fired {
    Out(HashMap<String, Tok>, HashMap<usize, Tok>),
    Pending,
    Cancelled,
    Stopped,
}

pub(crate) enum ScopeEnd {
    Done,
    Pending,
    Stopped,
}

#[derive(PartialEq, Clone, Copy)]
enum NS {
    Waiting,
    Running,
    Pending,
    Done,
}

const CONTROL: &[&str] = &["start", "end", "input", "output", "if", "switch", "parallel", "join", "set", "emit", "wait", "note", "comment"];

fn is_compound(n: &Node) -> bool {
    matches!(n.kind.as_str(), "loop" | "subgraph" | "for-each") || (n.kind == "parallel" && n.cfg_str("mode").unwrap_or("copies") != "fixed")
}

/// Does the engine know how to run this kind (its own, or given to it)?
pub(crate) fn runnable(kinds: &HashMap<String, Arc<dyn NodeKind>>, kind: &str) -> bool {
    CONTROL.contains(&kind) || matches!(kind, "loop" | "subgraph" | "for-each" | "approve" | "sub-workflow") || kinds.contains_key(kind)
}

fn all(plan: &Plan, n: usize, t: Tok) -> HashMap<String, Tok> {
    plan.ports[n].outputs.iter().map(|p| (p.name.clone(), if p.name == "error" { Tok::Skip } else { t.clone() })).collect()
}

fn default_for(node: &Node, p: &pgm_workflow_internal::types::PortSpec) -> Option<Value> {
    let set = |v: Option<&Value>| v.filter(|x| !x.is_null()).cloned();
    set(node.cfg(&p.name)).or_else(|| set(node.cfg("defaults").and_then(|d| d.get(&p.name)))).or_else(|| set(node.cfg("inputs").and_then(|d| d.get(&p.name)))).or_else(|| p.default.clone())
}

fn decide(run: &Run, scope: &Scope, n: usize) -> Decision {
    let plan = &scope.plan;
    let node = &plan.nodes[n];
    let kind = node.kind.as_str();
    if let Some(rec) = run.prior.memo.get(&scope.key(&node.id)) {
        if matches!(rec.get("status").and_then(Value::as_str), Some("skipped")) && rec.get("reason").and_then(Value::as_str) == Some("cancelled") {
            return Decision::Resolve(all(plan, n, Tok::Skip), None);
        }
    }
    let waits = &plan.wait[n];
    let imp = &plan.implicit[n];
    if waits.is_empty() && imp.is_empty() {
        return match kind {
            "start" if plan.key.is_none() => Decision::Fire(Inputs::default()),
            "input" => Decision::Fire(Inputs::default()),
            _ => Decision::Resolve(all(plan, n, Tok::Skip), None),
        };
    }
    let toks: Vec<Option<Tok>> = waits.iter().map(|&e| scope.resolve(e)).collect();
    if kind == "join" { return decide_join(plan, n, toks); }
    let err = toks.iter().find_map(|t| if let Some(Tok::Err(e)) = t { Some(e.clone()) } else { None });
    if let Some(e) = err {
        if kind == "end" || kind == "output" { return Decision::Fire(Inputs { err: Some(e), ..Default::default() }); }
        return Decision::Resolve(all(plan, n, Tok::Err(e)), Some("upstream-error"));
    }
    let any_val = toks.iter().any(|t| matches!(t, Some(Tok::Val(_)) | Some(Tok::Lines(_))));
    let complete = toks.iter().all(Option::is_some) && imp.iter().all(|&e| scope.resolve(e).is_some());
    if !(complete || kind == "end" && any_val) { return Decision::NotYet; }
    if !any_val { return Decision::Resolve(all(plan, n, Tok::Skip), Some("skipped")); }
    let mut per: HashMap<String, Vec<Value>> = HashMap::new();
    for (i, &e) in waits.iter().enumerate() {
        let port = plan.edges[e].dport.clone();
        match &toks[i] {
            Some(Tok::Val(v)) => per.entry(port).or_default().push(v.clone()),
            Some(Tok::Lines(ls)) => for t in ls { if let Tok::Val(v) = t { per.entry(port.clone()).or_default().push(v.clone()); } },
            _ => {}
        }
    }
    let mut values = Map::new();
    for p in &plan.ports[n].inputs {
        let v = match per.remove(&p.name) {
            Some(vs) if p.many => Some(Value::Array(vs)),
            Some(mut vs) => Some(vs.remove(0)),
            None => default_for(node, p),
        };
        match v {
            Some(v) => { values.insert(p.name.clone(), v); }
            None if p.required => return Decision::Resolve(all(plan, n, Tok::Skip), Some("skipped")),
            None => {}
        }
    }
    for (k, mut vs) in per { values.insert(k, if vs.len() == 1 { vs.remove(0) } else { Value::Array(vs) }); }
    Decision::Fire(Inputs { values, ..Default::default() })
}

fn decide_join(plan: &Plan, n: usize, toks: Vec<Option<Tok>>) -> Decision {
    let node = &plan.nodes[n];
    let mode = node.cfg_str("mode").unwrap_or("all");
    let on_fail = node.cfg_str("onFail").unwrap_or("fail");
    let need = node.cfg("n").and_then(Value::as_u64).unwrap_or(1) as usize;
    let mut lines: Vec<Option<Tok>> = vec![];
    for t in toks { match t { Some(Tok::Lines(v)) => lines.extend(v.into_iter().map(Some)), other => lines.push(other) } }
    let vals = lines.iter().filter(|t| matches!(t, Some(Tok::Val(_)))).count();
    let errs = lines.iter().any(|t| matches!(t, Some(Tok::Err(_))));
    let pending = lines.iter().filter(|t| t.is_none()).count();
    let fire = (errs && on_fail == "fail") || match mode { "first" | "any" => vals >= 1 || pending == 0, "n-of-m" => vals >= need || pending == 0, _ => pending == 0 };
    if !fire { return Decision::NotYet; }
    if vals == 0 && !errs { return Decision::Resolve(all(plan, n, Tok::Skip), Some("skipped")); }
    Decision::Fire(Inputs { lines, ..Default::default() })
}

fn log_skip(run: &Run, scope: &Scope, n: usize, reason: &str) {
    let node = &scope.plan.nodes[n];
    let key = scope.key(&node.id);
    if run.mode == Mode::Resume && run.prior.memo.contains_key(&key) { return; }
    run.log.emit("node_end", json!({ "node": node.id, "key": key, "kind": node.kind, "status": "skipped", "reason": reason, "graph": scope.plan.key }));
}

/// Records what a node gave; a failure with no wire to go along ends the run.
fn settle(run: &Run, scope: &Scope, n: usize, outs: HashMap<String, Tok>, escapes: HashMap<usize, Tok>) {
    let plan = &scope.plan;
    let err = outs.values().find_map(|t| if let Tok::Err(e) = t { Some(e.clone()) } else { None });
    if let Some(e) = err {
        let wired = plan.outs[n].iter().any(|&x| matches!(outs.get(&plan.edges[x].sport), Some(Tok::Err(_))));
        if !wired { run.halt(Stop::Fail(e)); }
    }
    lock(&scope.escapes).extend(escapes);
    lock(&scope.outs).insert(n, outs);
}

/// Runs one instance of a scope until every node in it has finished, it waits for a person, or the run stops.
pub(crate) fn exec_scope(run: Arc<Run>, scope: Arc<Scope>) -> BoxFuture<'static, ScopeEnd> {
    async move {
        let plan = scope.plan.clone();
        let members: Vec<usize> = plan.direct.get(&scope.region).cloned().unwrap_or_default();
        let mut st: HashMap<usize, NS> = members.iter().map(|&m| (m, NS::Waiting)).collect();
        let mut node_cancel: HashMap<usize, Cancel> = HashMap::new();
        let mut running = FuturesUnordered::new();
        loop {
            if run.stopped() || scope.cancel.is_cancelled() {
                while running.next().await.is_some() {}
                return ScopeEnd::Stopped;
            }
            let mut changed = true;
            while changed && !run.stopped() {
                changed = false;
                for &m in &members {
                    if st[&m] != NS::Waiting { continue; }
                    match decide(&run, &scope, m) {
                        Decision::NotYet => {}
                        Decision::Resolve(outs, reason) => {
                            if let Some(r) = reason { log_skip(&run, &scope, m, r); }
                            settle(&run, &scope, m, outs, HashMap::new());
                            st.insert(m, NS::Done);
                            changed = true;
                        }
                        Decision::Fire(inputs) => {
                            let c = scope.cancel.child();
                            node_cancel.insert(m, c.clone());
                            running.push(fire(run.clone(), scope.clone(), m, inputs, c));
                            st.insert(m, NS::Running);
                        }
                    }
                }
            }
            if running.is_empty() {
                if run.stopped() || scope.cancel.is_cancelled() { return ScopeEnd::Stopped; }
                if st.values().all(|s| *s == NS::Done) { return ScopeEnd::Done; }
                if st.values().any(|s| *s == NS::Pending) { return ScopeEnd::Pending; }
                let stuck: Vec<&str> = members.iter().filter(|m| st[m] == NS::Waiting).map(|&m| plan.nodes[m].id.as_str()).collect();
                run.halt(Stop::Fail(json!({ "code": "engine.stuck", "message": format!("nothing can run and {} still wait for inputs that cannot come", stuck.join(", ")) })));
                return ScopeEnd::Stopped;
            }
            let next = tokio::select! {
                r = running.next() => r,
                _ = scope.cancel.cancelled() => None,
            };
            let Some((m, fired)) = next else { continue };
            match fired {
                Fired::Out(outs, escapes) => {
                    settle(&run, &scope, m, outs, escapes);
                    st.insert(m, NS::Done);
                    if plan.nodes[m].kind == "join" {
                        // the join is satisfied: whatever only fed it is not needed any more
                        for &x in plan.feeds_only.get(&m).map(Vec::as_slice).unwrap_or(&[]) {
                            match st.get(&x) {
                                Some(NS::Running) => { if let Some(c) = node_cancel.get(&x) { c.cancel(); } }
                                Some(NS::Waiting) => {
                                    log_skip(&run, &scope, x, "cancelled");
                                    settle(&run, &scope, x, all(&plan, x, Tok::Skip), HashMap::new());
                                    st.insert(x, NS::Done);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Fired::Pending => { st.insert(m, NS::Pending); }
                Fired::Cancelled => {
                    settle(&run, &scope, m, all(&plan, m, Tok::Skip), HashMap::new());
                    st.insert(m, NS::Done);
                }
                Fired::Stopped => { st.insert(m, NS::Done); }
            }
        }
    }
    .boxed()
}

fn fire(run: Arc<Run>, scope: Arc<Scope>, n: usize, inp: Inputs, cancel: Cancel) -> BoxFuture<'static, (usize, Fired)> {
    async move {
        let f = AssertUnwindSafe(fire_inner(&run, &scope, n, inp, cancel)).catch_unwind().await;
        let f = match f {
            Ok(f) => f,
            Err(p) => {
                // a bug in the engine itself, not in a node (those are caught per attempt): that node fails, the hub goes on
                let node = &scope.plan.nodes[n];
                let e = err_value(&NodeError::new("engine.panic", panic_text(&p)), &node.id, 1);
                Fired::Out(all(&scope.plan, n, Tok::Err(e)), HashMap::new())
            }
        };
        (n, f)
    }
    .boxed()
}

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "a panic".into())
}

async fn fire_inner(run: &Arc<Run>, scope: &Arc<Scope>, n: usize, inp: Inputs, cancel: Cancel) -> Fired {
    let plan = scope.plan.clone();
    let node = &plan.nodes[n];
    let key = scope.key(&node.id);
    if !is_compound(node) {
        if let Some(rec) = run.prior.memo.get(&key).cloned() { return from_memo(run, scope, n, &key, &rec); }
    }
    let r = match node.kind.as_str() {
        // a custom `graph` kind (T-203): its graph, as a sub-graph
        k if run.engine.custom_graph(k).is_some() => run_sub(run, scope, n, &key, inp, cancel, Some(crate::engine::kind_graph_key(k))).await,
        "loop" => run_loop(run, scope, n, &key, inp, cancel).await,
        "parallel" if is_compound(node) => run_lines(run, scope, n, &key, inp, cancel).await,
        "subgraph" | "for-each" => run_sub(run, scope, n, &key, inp, cancel, None).await,
        "approve" => approve(run, scope, n, &key, inp),
        "sub-workflow" => run_child(run, scope, n, &key, inp, cancel).await,
        k if CONTROL.contains(&k) => control(run, scope, n, &key, inp, cancel).await,
        _ => leaf(run, scope, n, &key, inp, cancel).await,
    };
    match r { Ok(f) => f, Err(e) => fail_node(run, scope, n, &key, 1, &e, now(run)) }
}

fn now(run: &Run) -> u64 { run.host.now_ms() }

/// The outputs a finished instance had, from its `node_end` in an earlier log.
fn from_memo(run: &Run, scope: &Scope, n: usize, key: &str, rec: &Value) -> Fired {
    let plan = &scope.plan;
    let node = &plan.nodes[n];
    if run.mode == Mode::Replay {
        let mut s = json!({ "node": node.id, "key": key, "kind": node.kind, "attempt": rec.get("attempt").cloned().unwrap_or(json!(1)), "replayed": true, "graph": plan.key });
        if !run.count_run() { return Fired::Stopped; }
        run.log.emit("node_start", s.clone());
        if let (Some(o), Value::Object(r)) = (s.as_object_mut(), rec) {
            for (k, v) in r { if !matches!(k.as_str(), "v" | "seq" | "at" | "run" | "type") { o.insert(k.clone(), v.clone()); } }
        }
        if let Some(u) = rec.get("usage") { run.add_usage(u); }
        run.log.emit("node_end", s);
    }
    let outs_v = rec.get("outputs").and_then(Value::as_object).cloned().unwrap_or_default();
    let status = rec.get("status").and_then(Value::as_str).unwrap_or("ok");
    let fail = status == "error" && rec.get("handled").and_then(Value::as_str) == Some("fail");
    let errv = rec.get("error").cloned().unwrap_or(Value::Null);
    match node.kind.as_str() {
        "set" => { if let Some(v) = outs_v.get("out") { lock(&run.state).insert(node.cfg_str("key").unwrap_or(&node.id).to_string(), v.clone()); } }
        "end" => {
            if status == "error" { run.halt(Stop::Fail(errv.clone())); } else { run.halt(Stop::End { name: end_name(node), result: rec.get("result").cloned().unwrap_or(Value::Null) }); }
        }
        "output" => {
            let t = if status == "error" { Tok::Err(errv.clone()) } else { outs_v.get("value").cloned().map(Tok::Val).unwrap_or(Tok::Skip) };
            lock(&scope.sub_out).insert(out_name(node), t);
        }
        _ => {}
    }
    if status == "cancelled" || status == "skipped" { return Fired::Out(all(plan, n, Tok::Skip), HashMap::new()); }
    let outs = plan.ports[n].outputs.iter().map(|p| {
        let t = match outs_v.get(&p.name) {
            Some(v) => Tok::Val(v.clone()),
            None if fail && p.name != "error" => Tok::Err(errv.clone()),
            None => Tok::Skip,
        };
        (p.name.clone(), t)
    }).collect();
    Fired::Out(outs, HashMap::new())
}

fn end_name(node: &Node) -> String { node.cfg_str("name").unwrap_or("done").to_string() }
fn out_name(node: &Node) -> String { node.cfg_str("name").unwrap_or(&node.id).to_string() }

fn vals_of(outs: &HashMap<String, Tok>) -> Map<String, Value> {
    outs.iter().filter_map(|(k, t)| if let Tok::Val(v) = t { Some((k.clone(), v.clone())) } else { None }).collect()
}

/// `node_start` (counted against `maxNodes`). False when the run must stop instead.
fn begin(run: &Run, scope: &Scope, n: usize, key: &str, attempt: u32, inputs: &Map<String, Value>) -> bool { begin_with(run, scope, n, key, attempt, inputs, Value::Null) }

#[allow(clippy::too_many_arguments)]
fn begin_with(run: &Run, scope: &Scope, n: usize, key: &str, attempt: u32, inputs: &Map<String, Value>, more: Value) -> bool {
    if run.stopped() { return false; }
    let node = &scope.plan.nodes[n];
    if !run.count_run() { return false; }
    lock(&run.started).insert(key.to_string());
    let mut ev = json!({
        "node": node.id, "key": key, "kind": node.kind, "attempt": attempt, "iteration": scope.iteration, "line": scope.line,
        "inputs": inputs, "graph": scope.plan.key,
    });
    if let (Some(o), Value::Object(m)) = (ev.as_object_mut(), more) { for (k, v) in m { o.insert(k, v); } }
    run.log.emit("node_start", ev);
    true
}

/// The laws a node's AI follows (spec 6.2): the scope's standing laws, then the workflow's, then its sections' (outer
/// to inner), then its own; groups expanded, each law once (the first place stays). `-law-x` (or `-grp-x`) at a lower
/// level takes a higher level's law away, except one marked `lockable: false`, which stays.
pub(crate) fn node_laws(run: &Run, plan: &Plan, node: &Node) -> Vec<pgm_workflow_internal::shapes::Law> {
    let book = &run.laws;
    if book.laws.is_empty() { return vec![]; }
    let mut levels: Vec<Vec<String>> = vec![book.defaults.clone(), run.doc.defaults.as_ref().and_then(|d| d.laws.clone()).unwrap_or_default()];
    if plan.key.is_none() {
        let mut secs: Vec<_> = run.doc.sections.iter().flatten().filter(|s| s.nodes.contains(&node.id) && s.laws.is_some()).collect();
        secs.sort_by_key(|s| std::cmp::Reverse(s.nodes.len()));
        for s in secs { levels.push(s.laws.clone().unwrap_or_default()); }
    }
    levels.push(node.laws.clone().unwrap_or_default());
    let mut out: Vec<pgm_workflow_internal::shapes::Law> = vec![];
    for level in levels {
        for entry in level {
            let (remove, id) = match entry.strip_prefix('-') { Some(x) => (true, x.to_string()), None => (false, entry.clone()) };
            let (found, _) = pgm_workflow_internal::shapes::expand_laws(std::slice::from_ref(&id), &book.laws, &book.groups);
            for l in found {
                if remove {
                    if l.extra.get("lockable") != Some(&Value::Bool(false)) { out.retain(|x| x.id != l.id); }
                } else if !out.iter().any(|x| x.id == l.id) {
                    out.push(l.clone());
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn finish(run: &Run, scope: &Scope, n: usize, key: &str, attempt: u32, status: &str, outs: &HashMap<String, Tok>, started: u64, more: Value) {
    let node = &scope.plan.nodes[n];
    let mut ev = json!({
        "node": node.id, "key": key, "kind": node.kind, "attempt": attempt, "status": status, "outputs": vals_of(outs),
        "durationMs": now(run).saturating_sub(started), "graph": scope.plan.key,
    });
    if let (Some(o), Value::Object(m)) = (ev.as_object_mut(), more) { for (k, v) in m { o.insert(k, v); } }
    run.log.emit("node_end", ev);
}

/// A compound node (loop, lines, sub-graph) logs its start once, even when a resumed run goes through it again.
fn begin_compound(run: &Run, scope: &Scope, n: usize, key: &str, inputs: &Map<String, Value>) -> bool {
    if run.prior.started.contains_key(key) { return !run.stopped(); }
    begin(run, scope, n, key, 1, inputs)
}

// ---------------------------------------------------------------- settings and failures

struct Settings {
    timeout: Option<f64>,
    max: u32,
    backoff: f64,
    factor: f64,
    on: Vec<String>,
    on_error: String,
    default: Value,
}

fn settings(run: &Run, plan: &Plan, node: &Node) -> Settings {
    let doc = &run.doc;
    let get = |k: &str| -> Option<Value> {
        if let Some(v) = node.settings.as_ref().and_then(|s| s.get(k)) { return Some(v.clone()); }
        if plan.key.is_none() {
            for s in doc.sections.as_deref().unwrap_or(&[]) {
                if !s.nodes.contains(&node.id) { continue; }
                if let Some(v) = s.extra.get("settings").and_then(|x| x.get(k)).or_else(|| s.extra.get(k)) { return Some(v.clone()); }
            }
        }
        let d = doc.defaults.as_ref()?;
        match k {
            "retry" => d.retry.clone(),
            "onError" => d.on_error.clone().map(Value::String),
            _ => d.extra.get(k).cloned(),
        }
    };
    let retry = get("retry").unwrap_or(Value::Null);
    Settings {
        timeout: get("timeoutSec").and_then(|v| v.as_f64()).filter(|t| *t > 0.0),
        max: retry.get("max").and_then(Value::as_u64).unwrap_or(0).min(50) as u32,
        backoff: retry.get("backoffSec").and_then(Value::as_f64).unwrap_or(1.0).max(0.0),
        factor: retry.get("factor").and_then(Value::as_f64).unwrap_or(2.0).max(1.0),
        on: retry.get("on").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_else(|| vec!["timeout".into(), "error".into()]),
        on_error: get("onError").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "fail".into()),
        default: get("default").unwrap_or(Value::Null),
    }
}

/// A node that failed for good (after its retries): its `error` port when that is wired, else its `onError`.
fn fail_node(run: &Run, scope: &Scope, n: usize, key: &str, attempt: u32, e: &NodeError, started: u64) -> Fired {
    let plan = &scope.plan;
    let node = &plan.nodes[n];
    let ev = err_value(e, &node.id, attempt);
    let st = settings(run, plan, node);
    let (outs, more) = if plan.has_edge_from(n, "error") {
        let mut o = all(plan, n, Tok::Skip);
        o.insert("error".into(), Tok::Val(ev.clone()));
        (o, json!({ "error": ev, "port": "error" }))
    } else {
        match st.on_error.as_str() {
            "skip" => (all(plan, n, Tok::Val(Value::Null)), json!({ "error": ev, "handled": "skip", "warning": format!("node `{}` failed ({}); onError: skip goes on with an empty value", node.id, e.code) })),
            "continue-with-default" => (all(plan, n, Tok::Val(st.default.clone())), json!({ "error": ev, "handled": "default", "warning": format!("node `{}` failed ({}); goes on with its default", node.id, e.code) })),
            _ => (all(plan, n, Tok::Err(ev.clone())), json!({ "error": ev, "handled": "fail" })),
        }
    };
    if !run.prior.started.contains_key(key) && !lock(&run.started).contains(key) {
        // a failure before anything started (a bad config, a missing sub-graph): the log still pairs start and end
        if !begin(run, scope, n, key, attempt, &Map::new()) { return Fired::Stopped; }
    }
    finish(run, scope, n, key, attempt, "error", &outs, started, more);
    Fired::Out(outs, HashMap::new())
}

// ---------------------------------------------------------------- the engine's own kinds

fn look<'a>(vars: &'a Value, value: Option<&'a Value>, extra: &'a Map<String, Value>) -> impl Fn(&[String]) -> Option<Value> + 'a {
    move |p: &[String]| {
        let (first, rest) = p.split_first()?;
        if let Some(v) = extra.get(first) { return walk(v, rest); }
        if first == "value" { if let Some(v) = value { return walk(v, rest); } }
        walk(vars.get(first)?, rest)
    }
}

fn condition(run: &Run, scope: &Scope, node: &Node, src: &str, value: Option<&Value>, extra: &Map<String, Value>) -> Result<bool, NodeError> {
    let c = expr::parse_condition(src).map_err(|e| NodeError::fatal("engine.condition", format!("node `{}`: {e}", node.id)))?;
    let vars = scope.vars(run);
    let f = look(&vars, value, extra);
    c.test(&f).map_err(|e| NodeError::fatal("engine.condition", format!("node `{}`: {e}", node.id)))
}

fn as_text(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), x => x.to_string() } }

async fn control(run: &Arc<Run>, scope: &Arc<Scope>, n: usize, key: &str, inp: Inputs, cancel: Cancel) -> Result<Fired, NodeError> {
    let plan = &scope.plan;
    let node = &plan.nodes[n];
    let t0 = now(run);
    if !begin(run, scope, n, key, 1, &inp.values) { return Ok(Fired::Stopped); }
    let v = |p: &str| inp.values.get(p).cloned().unwrap_or(Value::Null);
    let mut outs = all(plan, n, Tok::Skip);
    let mut more = json!({});
    match node.kind.as_str() {
        "start" => {
            for p in ["prompt", "inputs", "files", "sender", "project"] {
                if let Some(x) = run.input.get(p).filter(|x| !x.is_null()) { outs.insert(p.into(), Tok::Val(x.clone())); }
            }
        }
        "input" => {
            let name = out_name(node);
            match scope.sub_in.get(&name).cloned().or_else(|| node.cfg("default").cloned()) {
                Some(x) => { outs.insert("value".into(), Tok::Val(x)); }
                None => { finish(run, scope, n, key, 1, "skipped", &outs, t0, json!({ "reason": "no value" })); return Ok(Fired::Out(outs, HashMap::new())); }
            }
        }
        "output" => {
            let t = match &inp.err { Some(e) => Tok::Err(e.clone()), None => Tok::Val(v("value")) };
            lock(&scope.sub_out).insert(out_name(node), t);
        }
        "end" => {
            if let Some(e) = &inp.err {
                finish(run, scope, n, key, 1, "error", &outs, t0, json!({ "error": e }));
                run.halt(Stop::Fail(e.clone()));
                return Ok(Fired::Out(outs, HashMap::new()));
            }
            let result = match v("result") { Value::Array(mut a) if a.len() == 1 => a.remove(0), Value::Array(a) if !a.is_empty() => a[0].clone(), x => x };
            finish(run, scope, n, key, 1, "ok", &outs, t0, json!({ "result": result }));
            run.halt(Stop::End { name: end_name(node), result });
            return Ok(Fired::Out(outs, HashMap::new()));
        }
        "if" => {
            let val = v("value");
            let yes = condition(run, scope, node, node.cfg_str("condition").unwrap_or("value"), Some(&val), &Map::new())?;
            outs.insert(if yes { "true" } else { "false" }.into(), Tok::Val(val));
        }
        "switch" => {
            let val = v("value");
            let mut port = "default".to_string();
            for (i, c) in node.cfg("cases").and_then(Value::as_array).cloned().unwrap_or_default().iter().enumerate() {
                let (name, hit) = match c {
                    Value::String(s) => (s.clone(), as_text(&val) == *s),
                    Value::Object(o) => {
                        let name = ["port", "name", "id"].iter().find_map(|k| o.get(*k).and_then(Value::as_str)).map(str::to_string).unwrap_or_else(|| format!("case{}", i + 1));
                        let hit = if let Some(x) = o.get("equals") { *x == val || as_text(x) == as_text(&val) && !x.is_object() }
                            else if let Some(p) = o.get("matches").and_then(Value::as_str) { expr::regex_search(p, &as_text(&val)).map_err(|e| NodeError::fatal("engine.condition", format!("node `{}`: {e}", node.id)))? }
                            else if let Some(Value::Array(l)) = o.get("in") { l.contains(&val) }
                            else if let Some(Value::Array(r)) = o.get("range") { val.as_f64().is_some_and(|x| r.first().and_then(Value::as_f64).is_none_or(|lo| x >= lo) && r.get(1).and_then(Value::as_f64).is_none_or(|hi| x <= hi)) }
                            else if let Some(c) = o.get("when").or_else(|| o.get("condition")).and_then(Value::as_str) { condition(run, scope, node, c, Some(&val), &Map::new())? }
                            else { false };
                        (name, hit)
                    }
                    _ => (format!("case{}", i + 1), false),
                };
                if hit { port = name; break; }
            }
            more = json!({ "port": port });
            outs.insert(port, Tok::Val(val));
        }
        "parallel" => {
            // `fixed`: every named line gets the value, the wires do the rest
            let val = v("in");
            for p in plan.ports[n].outputs.iter().filter(|p| p.name != "error") { outs.insert(p.name.clone(), Tok::Val(val.clone())); }
        }
        "join" => {
            let mode = node.cfg_str("mode").unwrap_or("all");
            let on_fail = node.cfg_str("onFail").unwrap_or("fail");
            let lines: Vec<Tok> = inp.lines.into_iter().map(|t| t.unwrap_or(Tok::Skip)).collect();
            if let Some(e) = lines.iter().find_map(|t| if let Tok::Err(e) = t { Some(e.clone()) } else { None }).filter(|_| on_fail == "fail") {
                if plan.has_edge_from(n, "error") { outs.insert("error".into(), Tok::Val(e.clone())); } else { outs.insert("result".into(), Tok::Err(e.clone())); }
                finish(run, scope, n, key, 1, "error", &outs, t0, json!({ "error": e, "handled": if plan.has_edge_from(n, "error") { Value::Null } else { json!("fail") } }));
                return Ok(Fired::Out(outs, HashMap::new()));
            }
            let mut list = vec![];
            let mut left_out = 0;
            for t in &lines {
                match t { Tok::Val(x) => list.push(x.clone()), Tok::Err(e) if on_fail == "collect" => list.push(e.clone()), Tok::Err(_) => left_out += 1, _ => {} }
            }
            let result = match mode {
                "first" | "any" => list.into_iter().next().unwrap_or(Value::Null),
                "n-of-m" => Value::Array(list.into_iter().take(node.cfg("n").and_then(Value::as_u64).unwrap_or(1) as usize).collect()),
                "merge" => Value::String(list.iter().map(as_text).collect::<Vec<_>>().join(node.cfg_str("separator").unwrap_or("\n\n"))),
                _ => Value::Array(list),
            };
            if left_out > 0 { more = json!({ "warning": format!("{left_out} failed line(s) left out (onFail: skip)") }); }
            outs.insert("result".into(), Tok::Val(result));
        }
        "set" => {
            let val = v("value");
            lock(&run.state).insert(node.cfg_str("key").unwrap_or(&node.id).to_string(), val.clone());
            outs.insert("out".into(), Tok::Val(val));
        }
        "emit" => {
            let msg = as_text(&v("message"));
            if run.mode != Mode::Replay { run.host.emit(&run.id, key, &msg); }
            outs.insert("done".into(), Tok::Val(Value::String(msg)));
        }
        "wait" => {
            let secs = node.cfg("seconds").and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
            if secs > 0.0 {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs_f64(secs)) => {}
                    _ = cancel.cancelled() => { finish(run, scope, n, key, 1, "cancelled", &all(plan, n, Tok::Skip), t0, json!({})); return Ok(Fired::Cancelled); }
                }
            }
            outs.insert("out".into(), Tok::Val(v("in")));
        }
        _ => {}
    }
    finish(run, scope, n, key, 1, "ok", &outs, t0, more);
    Ok(Fired::Out(outs, HashMap::new()))
}

fn approved(a: &Value) -> bool {
    if let Some(b) = a.get("approved").and_then(Value::as_bool) { return b; }
    let mut cur = a;
    loop {
        match cur {
            Value::Array(x) if !x.is_empty() => cur = &x[0],
            Value::Object(o) => match o.get("answer").or_else(|| o.get("answers")) { Some(x) => cur = x, None => return false },
            Value::String(s) => return matches!(s.trim().to_lowercase().as_str(), "yes" | "y" | "approve" | "approved" | "ok" | "accept" | "accepted" | "true"),
            Value::Bool(b) => return *b,
            _ => return false,
        }
    }
}

fn question_defaults(qs: &Value) -> Value {
    Value::Array(qs.as_array().map(|a| a.iter().map(|q| q.get("default").cloned().unwrap_or(Value::Null)).collect()).unwrap_or_default())
}

/// `approve` (spec 4.6): asks once, then the run waits with no worker held; an answer (or the deadline) wakes it.
fn approve(run: &Arc<Run>, scope: &Arc<Scope>, n: usize, key: &str, inp: Inputs) -> Result<Fired, NodeError> {
    let plan = &scope.plan;
    let node = &plan.nodes[n];
    let t0 = now(run);
    let input = inp.values.get("in").cloned().unwrap_or(Value::Null);
    let questions = node.cfg("questions").cloned().unwrap_or_else(|| json!([{ "text": node.label.clone().unwrap_or_else(|| "Approve?".into()), "kind": "single", "options": ["yes", "no"], "default": "no" }]));
    let wait = run.prior.waits.get(key).cloned();
    let answer = run.prior.answers.get(key).cloned();
    let push_wait = |w: &Value| lock(&run.waiting).push(json!({ "node": node.id, "key": key, "what": w.get("what"), "ref": w.get("ref"), "deadline": w.get("deadline") }));
    let (answers, by) = match (answer, wait) {
        (Some(a), _) => (a.get("answers").cloned().unwrap_or(Value::Null), a.get("by").and_then(Value::as_str).unwrap_or("user").to_string()),
        (None, Some(w)) => {
            let deadline = w.get("deadline").and_then(Value::as_u64);
            if !deadline.is_some_and(|d| now(run) >= d) { push_wait(&w); return Ok(Fired::Pending); }
            let (answers, by) = match node.cfg_str("onTimeout").unwrap_or("reject") {
                "approve" => (json!({ "approved": true }), "timeout"),
                "default" => (question_defaults(&questions), "default"),
                "fail" => return Err(NodeError::fatal("engine.timeout", format!("nobody answered `{}` in time", node.id))),
                _ => (json!({ "approved": false }), "timeout"),
            };
            run.log.emit("answer", json!({ "node": node.id, "key": key, "by": by, "answers": answers }));
            (answers, by.to_string())
        }
        (None, None) => {
            if run.mode == Mode::Replay { return Err(NodeError::fatal("engine.replay-missing", format!("node `{}` has no recorded answer to replay", node.id))); }
            if !begin(run, scope, n, key, 1, &inp.values) { return Ok(Fired::Stopped); }
            let deadline = node.cfg("timeoutSec").and_then(Value::as_f64).map(|s| t0 + (s * 1000.0) as u64);
            let ask = Ask { run: run.id.clone(), key: key.to_string(), node: node.id.clone(), who: node.cfg_str("who").unwrap_or("sender").to_string(), questions: questions.clone(), deadline, what: "approval".into() };
            let r = run.host.ask_person(&ask)?;
            let w = json!({ "node": node.id, "key": key, "what": "approval", "ref": r, "deadline": deadline });
            run.log.emit("wait", w.clone());
            push_wait(&w);
            return Ok(Fired::Pending);
        }
    };
    let mut outs = all(plan, n, Tok::Skip);
    outs.insert(if approved(&answers) { "approved" } else { "rejected" }.into(), Tok::Val(input));
    if let Some(t) = answers.get("text").and_then(Value::as_str) { outs.insert("text".into(), Tok::Val(json!(t))); }
    outs.insert("answer".into(), Tok::Val(answers));
    finish(run, scope, n, key, run.prior.started.get(key).map_or(1, |s| s.0), "ok", &outs, t0, json!({ "by": by }));
    Ok(Fired::Out(outs, HashMap::new()))
}

// ---------------------------------------------------------------- nodes given to the engine

async fn leaf(run: &Arc<Run>, scope: &Arc<Scope>, n: usize, key: &str, inp: Inputs, cancel: Cancel) -> Result<Fired, NodeError> {
    let plan = &scope.plan;
    let node = &plan.nodes[n];
    let Some(kind) = run.kinds.get(&node.kind).cloned() else {
        return Err(NodeError::fatal("engine.kind-missing", format!("this runner cannot run the kind `{}`", node.kind)));
    };
    if run.mode == Mode::Replay { return Err(NodeError::fatal("engine.replay-missing", format!("node `{}` ({key}) has no recorded result to replay", node.id))); }
    let st = settings(run, plan, node);
    let before = run.prior.started.get(key).map_or(0, |s| s.0);
    let mut answer = None;
    let mut asked = None;
    if let Some(w) = run.prior.waits.get(key) {
        // the node asked a person (or the engine did, for a cut node): no answer yet, it goes on waiting, asked once
        let Some(a) = run.prior.answers.get(key) else {
            lock(&run.waiting).push(json!({ "node": node.id, "key": key, "what": w.get("what"), "ref": w.get("ref"), "deadline": w.get("deadline") }));
            return Ok(Fired::Pending);
        };
        let answers = a.get("answers").cloned().unwrap_or(Value::Null);
        if w.get("what").and_then(Value::as_str) == Some("resume") {
            if !(approved(&answers) || answers.get("rerun").and_then(Value::as_bool) == Some(true)) { return Err(NodeError::fatal("engine.not-rerun", format!("node `{}` was cut mid-run and was not run again", node.id))); }
        } else {
            answer = Some(answers);
            asked = w.get("state").cloned();
        }
    } else if before > 0 && run.prior.in_flight(key) && !kind.idempotent(node) {
        // it was running when the run was cut, and it may not be repeated blindly: a person decides
        let qs = json!([{ "text": format!("`{}` was running when the run stopped and may not be safe to repeat. Run it again?", node.label.clone().unwrap_or(node.id.clone())), "kind": "single", "options": ["yes", "no"], "default": "no" }]);
        let ask = Ask { run: run.id.clone(), key: key.to_string(), node: node.id.clone(), who: "sender".into(), questions: qs, deadline: None, what: "resume".into() };
        let r = run.host.ask_person(&ask)?;
        let w = json!({ "node": node.id, "key": key, "what": "resume", "ref": r });
        run.log.emit("wait", w.clone());
        lock(&run.waiting).push(w);
        return Ok(Fired::Pending);
    }
    let _permit = tokio::select! {
        p = run.sem.clone().acquire_owned() => p.map_err(|_| NodeError::fatal("engine.closed", "the run is closing"))?,
        _ = cancel.cancelled() => return Ok(Fired::Cancelled),
    };
    let mut attempt = before;
    let mut tries = 0u32;
    loop {
        attempt += 1;
        tries += 1;
        let t0 = now(run);
        let laws = node_laws(run, plan, node);
        let law_ids: Value = if laws.is_empty() { Value::Null } else { json!(laws.iter().map(|l| json!({ "id": l.id, "v": l.version })).collect::<Vec<_>>()) };
        if !begin_with(run, scope, n, key, attempt, &inp.values, json!({ "laws": law_ids })) { return Ok(Fired::Stopped); }
        let warnings = Arc::new(Mutex::new(vec![]));
        let call = NodeCall {
            run: run.id.clone(), key: key.to_string(), node: node.clone(), attempt, inputs: inp.values.clone(), vars: scope.vars(run),
            doc: run.doc.clone(), graph: plan.key.clone(), cancel: cancel.clone(), answer: answer.clone(), asked: asked.clone(), laws, warnings: warnings.clone(),
        };
        let fut = AssertUnwindSafe(kind.run(call, &*run.host)).catch_unwind();
        let res = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                finish(run, scope, n, key, attempt, "cancelled", &all(plan, n, Tok::Skip), t0, json!({}));
                return Ok(Fired::Cancelled);
            }
            r = async {
                match st.timeout {
                    Some(t) => tokio::time::timeout(Duration::from_secs_f64(t), fut).await.unwrap_or_else(|_| Ok(Err(NodeError::new("engine.timeout", format!("node `{}` took longer than {t} s", node.id))))),
                    None => fut.await,
                }
            } => r,
        };
        if cancel.is_cancelled() {
            // the node noticed the cancel first and answered with an error of its own: still a cancel, not a failure
            finish(run, scope, n, key, attempt, "cancelled", &all(plan, n, Tok::Skip), t0, json!({}));
            return Ok(Fired::Cancelled);
        }
        let res = match res {
            Ok(r) => r,
            Err(p) => Err(NodeError::new("engine.panic", format!("node `{}` panicked: {}", node.id, panic_text(&p)))),
        };
        let (e, reason) = match res {
            Ok(out) => {
                if let Some(u) = &out.usage { run.add_usage(u); }
                let empty = out.outputs.values().all(|v| v.is_null() || v.as_str() == Some(""));
                if !(empty && st.on.iter().any(|o| o == "empty")) {
                    let mut outs = all(plan, n, Tok::Skip);
                    for (k, v) in out.outputs { outs.insert(k, Tok::Val(v)); }
                    let w = lock(&warnings).clone();
                    finish(run, scope, n, key, attempt, "ok", &outs, t0, json!({ "usage": out.usage, "warnings": if w.is_empty() { Value::Null } else { json!(w) } }));
                    return Ok(Fired::Out(outs, HashMap::new()));
                }
                (NodeError::new("engine.empty", format!("node `{}` gave an empty answer", node.id)), "empty")
            }
            Err(e) if e.code == "engine.ask" && e.ask.is_some() => {
                // not a failure: the node needs a person; the run waits with this attempt closed and no worker held
                let a = e.ask.clone().map(|b| *b).unwrap_or(Value::Null);
                let ask = Ask { run: run.id.clone(), key: key.to_string(), node: node.id.clone(), who: node.cfg_str("who").unwrap_or("sender").to_string(), questions: a.get("questions").cloned().unwrap_or(Value::Null), deadline: None, what: "decision".into() };
                let r = match run.host.ask_person(&ask) { Ok(r) => r, Err(e) => return Ok(fail_node(run, scope, n, key, attempt, &e, t0)) };
                finish(run, scope, n, key, attempt, "waiting", &all(plan, n, Tok::Skip), t0, json!({}));
                let w = json!({ "node": node.id, "key": key, "what": "decision", "ref": r, "state": a.get("state") });
                run.log.emit("wait", w.clone());
                lock(&run.waiting).push(json!({ "node": node.id, "key": key, "what": "decision", "ref": r }));
                return Ok(Fired::Pending);
            }
            Err(e) => { let r = if e.code == "engine.timeout" { "timeout" } else { "error" }; (e, r) }
        };
        let retry = e.retryable != Some(false) && tries <= st.max && st.on.iter().any(|o| o == reason) && !run.stopped();
        if !retry { return Ok(fail_node(run, scope, n, key, attempt, &e, t0)); }
        finish(run, scope, n, key, attempt, "error", &all(plan, n, Tok::Skip), t0, json!({ "error": err_value(&e, &node.id, attempt), "retry": true }));
        let after = st.backoff * st.factor.powi(tries as i32 - 1);
        run.log.emit("node_retry", json!({ "node": node.id, "key": key, "attempt": attempt + 1, "reason": reason, "afterMs": (after * 1000.0) as u64 }));
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs_f64(after)) => {}
            _ = cancel.cancelled() => return Ok(Fired::Cancelled),
        }
    }
}

// ---------------------------------------------------------------- loops, lines, sub-graphs

async fn run_loop(run: &Arc<Run>, scope: &Arc<Scope>, n: usize, key: &str, inp: Inputs, cancel: Cancel) -> Result<Fired, NodeError> {
    let plan = scope.plan.clone();
    let node = &plan.nodes[n];
    let r = plan.owned[&n];
    let region = plan.regions[r].clone();
    if !begin_compound(run, scope, n, key, &inp.values) { return Ok(Fired::Stopped); }
    let t0 = run.prior.started.get(key).map(|s| s.1).unwrap_or_else(|| now(run));
    let cost0 = run.cost();
    let mode = node.cfg_str("mode").unwrap_or("until");
    let cond = node.cfg_str("condition").unwrap_or("false").to_string();
    let count = node.cfg("count").and_then(Value::as_u64).unwrap_or(1);
    let max_it = node.cfg("maxIterations").and_then(Value::as_u64).unwrap_or(if mode == "count" { count } else { 10 }).min(100);
    let max_secs = node.cfg("maxSeconds").and_then(Value::as_f64);
    let max_cost = node.cfg("maxCost").and_then(Value::as_f64);
    let mut state = inp.values.get("in").cloned().unwrap_or(Value::Null);
    let mut round = 0u64;
    let mut last: Option<Arc<Scope>> = None;
    let test = |state: &Value, round: u64| -> Result<bool, NodeError> {
        let extra: Map<String, Value> = [("round".to_string(), json!(round)), ("loop".to_string(), json!({ "round": round, "state": state }))].into_iter().collect();
        condition(run, scope, node, &cond, Some(state), &extra)
    };
    let outcome: Result<(&str, Option<&str>), Value> = loop {
        let go = match mode { "count" => round < count, "while" => test(&state, round)?, _ => round == 0 || !test(&state, round)? };
        if !go { break Ok(("done", None)); }
        if round >= max_it { break Ok(("limit", Some("maxIterations"))); }
        if max_secs.is_some_and(|s| now(run).saturating_sub(t0) as f64 > s * 1000.0) { break Ok(("limit", Some("maxSeconds"))); }
        if max_cost.is_some_and(|c| run.cost() - cost0 > c) { break Ok(("limit", Some("maxCost"))); }
        round += 1;
        let entry: HashMap<String, Tok> = [("again".to_string(), Tok::Val(state.clone()))].into_iter().collect();
        let extra: Map<String, Value> = [("loop".to_string(), json!({ "round": round, "state": state }))].into_iter().collect();
        let inst = Scope::region(scope, r, format!("{}:{round}", node.id), entry, extra, Some(round), None, cancel.child());
        match exec_scope(run.clone(), inst.clone()).await {
            ScopeEnd::Pending => return Ok(Fired::Pending),
            ScopeEnd::Stopped => return Ok(if run.stopped() { Fired::Stopped } else { Fired::Cancelled }),
            ScopeEnd::Done => {}
        }
        let backs: Vec<Tok> = region.back.iter().map(|&e| inst.resolve(e).unwrap_or(Tok::Skip)).collect();
        last = Some(inst);
        if let Some(e) = backs.iter().find_map(|t| if let Tok::Err(e) = t { Some(e.clone()) } else { None }) { break Err(e); }
        let mut vals: Vec<Value> = backs.into_iter().filter_map(|t| if let Tok::Val(v) = t { Some(v) } else { None }).collect();
        if vals.is_empty() { break Ok(("done", None)); }
        state = if vals.len() == 1 { vals.remove(0) } else { Value::Array(vals) };
    };
    let escapes: HashMap<usize, Tok> = region.exits.iter().map(|&e| (e, last.as_ref().and_then(|i| i.resolve(e)).unwrap_or(Tok::Skip))).collect();
    let mut outs = all(&plan, n, Tok::Skip);
    match outcome {
        Ok((port, lim)) => {
            if let Some(l) = lim {
                run.log.emit("limit", json!({ "limit": l, "node": node.id, "key": key, "value": match l { "maxIterations" => json!(max_it), "maxSeconds" => json!(max_secs), _ => json!(max_cost) } }));
                if !plan.has_edge_from(n, "limit") {
                    let e = json!({ "code": "engine.gave-up", "message": format!("loop `{}` reached its {l} after {round} round(s) and its `limit` port is not wired", node.id), "node": node.id });
                    finish(run, scope, n, key, 1, "error", &outs, t0, json!({ "error": e }));
                    run.halt(Stop::GaveUp(e));
                    return Ok(Fired::Stopped);
                }
            }
            outs.insert(port.into(), Tok::Val(state));
            outs.insert("round".into(), Tok::Val(json!(round)));
            finish(run, scope, n, key, 1, "ok", &outs, t0, json!({ "rounds": round }));
        }
        Err(e) => {
            if plan.has_edge_from(n, "error") { outs.insert("error".into(), Tok::Val(e.clone())); } else { outs = all(&plan, n, Tok::Err(e.clone())); }
            finish(run, scope, n, key, 1, "error", &outs, t0, json!({ "error": e, "rounds": round, "handled": if plan.has_edge_from(n, "error") { Value::Null } else { json!("fail") } }));
        }
    }
    Ok(Fired::Out(outs, escapes))
}

async fn run_lines(run: &Arc<Run>, scope: &Arc<Scope>, n: usize, key: &str, inp: Inputs, cancel: Cancel) -> Result<Fired, NodeError> {
    let plan = scope.plan.clone();
    let node = &plan.nodes[n];
    let r = plan.owned[&n];
    let region = plan.regions[r].clone();
    let RKind::Lines { join } = region.kind else { return Err(NodeError::fatal("engine.bug", "lines without a join")) };
    let jn = &plan.nodes[join];
    let (jmode, jfail, jneed) = (jn.cfg_str("mode").unwrap_or("all"), jn.cfg_str("onFail").unwrap_or("fail"), jn.cfg("n").and_then(Value::as_u64).unwrap_or(1) as usize);
    if !begin_compound(run, scope, n, key, &inp.values) { return Ok(Fired::Stopped); }
    let t0 = now(run);
    let input = inp.values.get("in").cloned().unwrap_or(Value::Null);
    let items: Vec<Value> = match node.cfg_str("mode").unwrap_or("copies") {
        "split" => match input { Value::Array(a) => a, Value::Null => vec![], x => vec![x] },
        _ => vec![input; node.cfg("n").and_then(Value::as_u64).unwrap_or(2) as usize],
    };
    let mut insts = vec![];
    let mut futs = FuturesUnordered::new();
    for (i, item) in items.iter().enumerate() {
        let entry: HashMap<String, Tok> = [("out".to_string(), Tok::Val(item.clone()))].into_iter().collect();
        let extra: Map<String, Value> = [("item".to_string(), item.clone()), ("line".to_string(), json!(i + 1))].into_iter().collect();
        let inst = Scope::region(scope, r, format!("{}:l{}", node.id, i + 1), entry, extra, None, Some(format!("l{}", i + 1)), cancel.child());
        insts.push(inst.clone());
        let run = run.clone();
        futs.push(async move { (i, exec_scope(run, inst).await) }.boxed());
    }
    let mut res: Vec<Option<Tok>> = vec![None; items.len()];
    let mut pending = false;
    let to_join: Vec<usize> = region.exits.iter().copied().filter(|&e| plan.edges[e].dst == join).collect();
    while let Some((i, end)) = futs.next().await {
        let t = match end {
            ScopeEnd::Pending => { pending = true; continue; }
            ScopeEnd::Stopped => Tok::Skip,
            ScopeEnd::Done => to_join.iter().map(|&e| insts[i].resolve(e).unwrap_or(Tok::Skip)).find(|t| *t != Tok::Skip).unwrap_or(Tok::Skip),
        };
        let failed = matches!(t, Tok::Err(_));
        res[i] = Some(t);
        let vals = res.iter().filter(|t| matches!(t, Some(Tok::Val(_)))).count();
        let enough = match jmode { "first" | "any" => vals >= 1, "n-of-m" => vals >= jneed, _ => false } || failed && jfail == "fail";
        if enough { for (k, inst) in insts.iter().enumerate() { if res[k].is_none() { inst.cancel.cancel(); } } }
    }
    if run.stopped() { return Ok(Fired::Stopped); }
    if pending { return Ok(Fired::Pending); }
    let lines: Vec<Tok> = res.into_iter().map(|t| t.unwrap_or(Tok::Skip)).collect();
    let escapes: HashMap<usize, Tok> = to_join.iter().enumerate().map(|(k, &e)| (e, if k == 0 { Tok::Lines(lines.clone()) } else { Tok::Skip })).collect();
    let outs = all(&plan, n, Tok::Skip);
    finish(run, scope, n, key, 1, "ok", &outs, t0, json!({ "lines": items.len() }));
    Ok(Fired::Out(outs, escapes))
}

/// `sub-workflow` (spec 5.5): another workflow, by name and version range, from the host's store, as a run of its own
/// (id `<run>.<node key>`, its log linked to this one by `parent`). Nested at most [`pgm_workflow_internal::MAX_DEPTH`]
/// deep. A child that stops to wait for a person is not supported yet (`engine.child-waits`).
async fn run_child(run: &Arc<Run>, scope: &Arc<Scope>, n: usize, key: &str, inp: Inputs, cancel: Cancel) -> Result<Fired, NodeError> {
    let plan = scope.plan.clone();
    let node = &plan.nodes[n];
    let t0 = now(run);
    if run.mode == Mode::Replay { return Err(NodeError::fatal("engine.replay-missing", format!("node `{}` has no recorded result to replay", node.id))); }
    let name = node.cfg_str("name").unwrap_or("").to_string();
    let range = node.cfg_str("range").map(str::to_string);
    if run.depth + 1 > pgm_workflow_internal::MAX_DEPTH { return Err(NodeError::fatal("engine.depth", format!("node `{}` would run workflows more than {} deep", node.id, pgm_workflow_internal::MAX_DEPTH))); }
    let child = run.host.workflow(&name, range.as_deref()).ok_or_else(|| NodeError::fatal("engine.no-workflow", format!("node `{}`: there is no workflow `{name}`{} to run", node.id, range.as_deref().map(|r| format!(" @{r}")).unwrap_or_default())))?;
    if !begin(run, scope, n, key, 1, &inp.values) { return Ok(Fired::Stopped); }
    let mut input = crate::RunInput::prompt(&as_text(inp.values.get("in").unwrap_or(&Value::Null)));
    input.sender = json!({ "kind": "workflow", "id": run.id });
    input.project = run.input.get("project").and_then(Value::as_str).map(str::to_string);
    for (k, v) in node.cfg("inputs").and_then(Value::as_object).cloned().unwrap_or_default() {
        input.inputs.insert(k.clone(), inp.values.get(&k).cloned().unwrap_or(v));
    }
    let id: String = format!("{}.{key}", run.id).chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '-' }).collect();
    let opts = crate::RunOptions {
        run_id: Some(id.clone()), cancel: cancel.child(),
        limits: crate::RunLimits { max_seconds: run.limits.max_seconds, max_nodes: Some(run.limits.max_nodes), max_cost: run.limits.max_cost, max_parallel: Some(run.limits.max_parallel) },
        parent: Some(json!({ "run": run.id, "node": node.id, "key": key, "depth": run.depth + 1 })), ..Default::default()
    };
    let rep = Box::pin(run.engine.start(Arc::new(child.clone()), input, run.host.clone(), opts)).await;
    let link = json!({ "run": id, "workflow": child.name, "version": child.version, "status": rep.status, "outcome": rep.outcome });
    if let Some(u) = rep.totals.get("cost").and_then(Value::as_f64).filter(|c| *c > 0.0) { run.add_usage(&json!({ "cost": u })); }
    match rep.status.as_str() {
        "ok" => {
            let mut outs = all(&plan, n, Tok::Skip);
            outs.insert("result".into(), Tok::Val(rep.result.unwrap_or(Value::Null)));
            outs.insert("outcome".into(), Tok::Val(json!(rep.outcome)));
            finish(run, scope, n, key, 1, "ok", &outs, t0, json!({ "child": link }));
            Ok(Fired::Out(outs, HashMap::new()))
        }
        "cancelled" if cancel.is_cancelled() => { finish(run, scope, n, key, 1, "cancelled", &all(&plan, n, Tok::Skip), t0, json!({ "child": link })); Ok(Fired::Cancelled) }
        "waiting" => Err(NodeError::fatal("engine.child-waits", format!("node `{}`: the workflow `{name}` waits for a person, which a sub-workflow cannot do yet", node.id))),
        "invalid" => Err(NodeError::fatal("engine.child-invalid", format!("node `{}`: the workflow `{name}` cannot run: {}", node.id, rep.problems.iter().filter_map(|p| p.get("message").and_then(Value::as_str)).collect::<Vec<_>>().join("; ")))),
        _ => {
            let e = rep.error.unwrap_or(Value::Null);
            Err(NodeError::new(e.get("code").and_then(Value::as_str).unwrap_or("engine.child-failed"), format!("node `{}`: the workflow `{name}` ended {}: {}", node.id, rep.status, e.get("message").and_then(Value::as_str).unwrap_or(""))))
        }
    }
}

/// `subgraph` (a box you can look inside), a custom `graph` kind (`graph_ref`: its graph), and `for-each` (the same
/// sub-graph once per item, a list of results).
async fn run_sub(run: &Arc<Run>, scope: &Arc<Scope>, n: usize, key: &str, inp: Inputs, cancel: Cancel, graph_ref: Option<String>) -> Result<Fired, NodeError> {
    let plan = scope.plan.clone();
    let node = &plan.nodes[n];
    let t0 = now(run);
    let boxed = graph_ref.is_some() || node.kind == "subgraph";
    let sk = match &graph_ref { Some(r) => r.clone(), None => node.cfg_str("ref").ok_or_else(|| NodeError::fatal("engine.config", format!("node `{}` names no sub-graph (`ref`)", node.id)))?.to_string() };
    let sub = run.plans.get(&Some(sk.clone())).cloned().ok_or_else(|| NodeError::fatal("engine.config", format!("node `{}`: there is no sub-graph `{sk}`", node.id)))?;
    if scope.depth + 1 > pgm_workflow_internal::MAX_DEPTH { return Err(NodeError::fatal("engine.depth", format!("node `{}` opens sub-graphs more than {} deep", node.id, pgm_workflow_internal::MAX_DEPTH))); }
    if !begin_compound(run, scope, n, key, &inp.values) { return Ok(Fired::Stopped); }
    let out_names: Vec<String> = plan.ports[n].outputs.iter().filter(|p| p.name != "error").map(|p| p.name.clone()).collect();
    let mut outs = all(&plan, n, Tok::Skip);
    if boxed {
        let inst = Scope::graph(sub, scope.child_path(&format!("{}:sg", node.id)), scope.depth + 1, cancel.child(), inp.values.clone(), Map::new());
        match exec_scope(run.clone(), inst.clone()).await {
            ScopeEnd::Pending => return Ok(Fired::Pending),
            ScopeEnd::Stopped => return Ok(if run.stopped() { Fired::Stopped } else { Fired::Cancelled }),
            ScopeEnd::Done => {}
        }
        let got = lock(&inst.sub_out).clone();
        for name in &out_names { outs.insert(name.clone(), got.get(name).cloned().unwrap_or(Tok::Skip)); }
        if let Some(e) = outs.values().find_map(|t| if let Tok::Err(e) = t { Some(e.clone()) } else { None }) {
            if plan.has_edge_from(n, "error") { outs = all(&plan, n, Tok::Skip); outs.insert("error".into(), Tok::Val(e.clone())); }
            finish(run, scope, n, key, 1, "error", &outs, t0, json!({ "error": e }));
        } else {
            finish(run, scope, n, key, 1, "ok", &outs, t0, json!({}));
        }
        return Ok(Fired::Out(outs, HashMap::new()));
    }
    // for-each
    let items: Vec<Value> = match inp.values.get("items").cloned().unwrap_or(Value::Null) { Value::Array(a) => a, Value::Null => vec![], x => vec![x] };
    let in_name = sub.nodes.iter().find(|x| x.kind == "input").map(out_name).unwrap_or_else(|| "item".into());
    let on_fail = node.cfg_str("onFail").unwrap_or("fail");
    let mut futs = FuturesUnordered::new();
    let mut insts = vec![];
    for (i, item) in items.iter().enumerate() {
        let sub_in: Map<String, Value> = [(in_name.clone(), item.clone()), ("index".to_string(), json!(i))].into_iter().collect();
        let extra: Map<String, Value> = [("item".to_string(), item.clone())].into_iter().collect();
        let inst = Scope::graph(sub.clone(), scope.child_path(&format!("{}:{}", node.id, i + 1)), scope.depth + 1, cancel.child(), sub_in, extra);
        insts.push(inst.clone());
        let run = run.clone();
        futs.push(async move { (i, exec_scope(run, inst).await) }.boxed());
    }
    let mut res: Vec<Option<Tok>> = vec![None; items.len()];
    let mut pending = false;
    while let Some((i, end)) = futs.next().await {
        match end {
            ScopeEnd::Pending => pending = true,
            ScopeEnd::Stopped => res[i] = Some(Tok::Skip),
            ScopeEnd::Done => {
                let got = lock(&insts[i].sub_out).clone();
                let t = got.get("result").cloned().or_else(|| sub.nodes.iter().find(|x| x.kind == "output").and_then(|o| got.get(&out_name(o)).cloned())).unwrap_or(Tok::Skip);
                if matches!(t, Tok::Err(_)) && on_fail == "fail" { for (k, inst) in insts.iter().enumerate() { if k != i && res[k].is_none() { inst.cancel.cancel(); } } }
                res[i] = Some(t);
            }
        }
    }
    if run.stopped() { return Ok(Fired::Stopped); }
    if pending { return Ok(Fired::Pending); }
    let toks: Vec<Tok> = res.into_iter().map(|t| t.unwrap_or(Tok::Skip)).collect();
    if let Some(e) = toks.iter().find_map(|t| if let Tok::Err(e) = t { Some(e.clone()) } else { None }).filter(|_| on_fail == "fail") {
        return Ok(fail_node(run, scope, n, key, 1, &NodeError::new(e.get("code").and_then(Value::as_str).unwrap_or("engine.error"), e.get("message").and_then(Value::as_str).unwrap_or("an item failed")), t0));
    }
    let list: Vec<Value> = toks.into_iter().filter_map(|t| match t { Tok::Val(v) => Some(v), Tok::Err(e) if on_fail == "collect" => Some(e), _ => None }).collect();
    outs.insert("results".into(), Tok::Val(Value::Array(list)));
    finish(run, scope, n, key, 1, "ok", &outs, t0, json!({ "items": items.len() }));
    Ok(Fired::Out(outs, HashMap::new()))
}
