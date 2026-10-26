//! Validation: everything the library checks before anything may run (spec 3.3).
//!
//! [`validate`] gives a [`Report`]: a list of [`Issue`]s, each an `error` or a `warn`, with a stable `code` and a
//! message that names the node / port / edge (and the sub-graph, when the problem is inside one). Saving a draft with
//! errors is allowed; publishing one is not ([`Report::is_valid`], [`crate::version::publish`]).
//!
//! Rules (3.3) and where they are:
//! 1. one `start` (top level), an `end`: `start.missing` `start.multiple` `end.missing` `start.in-subgraph`
//! 2. edges join existing ports, types fit, no input with two edges unless `many`: `edge.endpoint` `edge.node`
//!    `edge.port` `edge.type` `input.multiple`
//! 3. a required input is fed or has a default: `input.required`
//! 4. a node unreachable from `start` is a warning: `node.unreachable`
//! 5. a cycle must pass through a `loop` node: `graph.cycle`
//! 6. every loop has a limit, every retry a maximum, every wait a timeout: `loop.no-limit` `loop.limit` `loop.condition`
//!    `retry.max` `wait.timeout` `approve.timeout`
//! 7. sub-graph ports match their inside, no self-containing sub-graph, depth at most 8: `subgraph.ports`
//!    `subgraph.recursive` `subgraph.depth` `subgraph.missing`
//! 8. the kind is known, its version installed, its config fits: `kind.unknown` `kind.version` `config.invalid`
//! 9. a law or saved prompt that is referenced exists: `law.missing` `prompt.missing`
//! 10. a model the scope cannot reach is a warning: `model.unreachable`
//!
//! Also checked: duplicate ids, sections naming nodes that are not there, templates and conditions that do not parse,
//! `{{node.x.out.y}}` that names nothing.
use crate::expr;
use crate::kinds::KindRegistry;
use crate::model::{Document, Edge, Node, PortDecl, Subgraph};
use crate::types::{split_endpoint, PortSpec, PortType, Ports};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level { Error, Warn }

/// `{level, node?, port?, edge?, code, message}` (spec 3.3); `graph` names the sub-graph the problem is in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Issue {
    pub level: Level,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
}

impl Issue {
    fn new(level: Level, code: &str, message: String) -> Issue { Issue { level, code: code.to_string(), message, node: None, port: None, edge: None, graph: None } }
    fn node(mut self, n: &str) -> Issue { self.node = Some(n.to_string()); self }
    fn port(mut self, p: &str) -> Issue { self.port = Some(p.to_string()); self }
    fn edge(mut self, e: &str) -> Issue { self.edge = Some(e.to_string()); self }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Report { pub issues: Vec<Issue> }

impl Report {
    pub fn errors(&self) -> impl Iterator<Item = &Issue> { self.issues.iter().filter(|i| i.level == Level::Error) }
    pub fn warnings(&self) -> impl Iterator<Item = &Issue> { self.issues.iter().filter(|i| i.level == Level::Warn) }
    /// No errors (warnings do not count): may be published.
    pub fn is_valid(&self) -> bool { self.errors().next().is_none() }
    /// Is there an issue with this code?
    pub fn has(&self, code: &str) -> bool { self.issues.iter().any(|i| i.code == code) }
    pub fn with_code(&self, code: &str) -> Vec<&Issue> { self.issues.iter().filter(|i| i.code == code).collect() }
    pub fn to_json(&self) -> Value { serde_json::to_value(&self.issues).unwrap_or(Value::Null) }
}

/// What the scope the workflow lives in knows (spec 3.3 rules 9 and 10, sections 6 and 7). The defaults say
/// "everything exists and is reachable", so a caller without a store gets no false errors.
pub trait ScopeInfo {
    /// A law id or a group id that exists and is visible in the workflow's scope.
    fn law_visible(&self, _id: &str) -> bool { true }
    /// A saved prompt (by name or id) that exists and is visible.
    fn prompt_visible(&self, _id: &str) -> bool { true }
    /// A model entry (`{via, provider, model ...}`) this scope can reach.
    fn model_reachable(&self, _model: &Value) -> bool { true }
}

pub struct Options<'a> {
    pub registry: &'a dyn KindRegistry,
    pub scope: Option<&'a dyn ScopeInfo>,
    /// Sub-graphs may nest this deep (default [`crate::MAX_DEPTH`]).
    pub max_depth: usize,
    pub max_nodes: usize,
}

impl<'a> Options<'a> {
    pub fn new(registry: &'a dyn KindRegistry) -> Self { Options { registry, scope: None, max_depth: crate::MAX_DEPTH, max_nodes: crate::MAX_NODES } }
    pub fn scope(mut self, s: &'a dyn ScopeInfo) -> Self { self.scope = Some(s); self }
}

/// Check a document against every rule of spec 3.3.
pub fn validate(doc: &Document, opts: &Options) -> Report {
    let mut v = V { doc, o: opts, issues: vec![] };
    v.run();
    Report { issues: v.issues }
}

struct V<'a> { doc: &'a Document, o: &'a Options<'a>, issues: Vec<Issue> }

/// One graph level: the top level or one sub-graph.
#[derive(Clone, Copy)]
struct G<'a> { nodes: &'a [Node], edges: &'a [Edge], key: Option<&'a str> }

const DEFAULT_ON_ERROR: [&str; 3] = ["fail", "skip", "continue-with-default"];

fn decl_ports(ds: &[PortDecl], input: bool) -> Vec<PortSpec> {
    ds.iter().map(|d| {
        let mut s = PortSpec::new(&d.name, PortType::parse(&d.ty).unwrap_or(PortType::Json));
        s.default = d.default.clone();
        s.required = input && d.required.unwrap_or(d.default.is_none());
        s
    }).collect()
}

impl<'a> V<'a> {
    fn push(&mut self, mut i: Issue, g: Option<&str>) { i.graph = g.map(str::to_string); self.issues.push(i); }

    fn run(&mut self) {
        let doc = self.doc;
        let top = G { nodes: &doc.nodes, edges: &doc.edges, key: None };
        self.decls(doc.inputs.as_deref().unwrap_or(&[]), "workflow input", None);
        self.decls(doc.outputs.as_deref().unwrap_or(&[]), "workflow output", None);
        if let Some(d) = &doc.defaults {
            self.law_refs(d.laws.as_deref(), "the workflow's defaults", None, None);
            self.prompt_refs(d.prompts.as_deref(), "the workflow's defaults", None, None);
            if let Some(m) = &d.model { self.model(m, "the workflow's default model", None, None); }
            if let Some(r) = &d.retry { self.retry(r, "the workflow's default retry", None, None); }
            if let Some(e) = &d.on_error { if !DEFAULT_ON_ERROR.contains(&e.as_str()) { let i = Issue::new(Level::Error, "onError.invalid",format!("the workflow's default onError `{e}` must be one of fail, skip, continue-with-default")); self.push(i, None); } }
        }
        self.graph(top);
        self.subgraph_tree();
        if let Some(subs) = &doc.subgraphs {
            for (k, s) in subs {
                self.graph(G { nodes: &s.nodes, edges: &s.edges, key: Some(k) });
                self.subgraph_ports(k, s);
            }
        }
    }

    // ----- rule 7: nesting, recursion, ports

    fn subgraph_tree(&mut self) {
        let doc = self.doc;
        let mut memo: HashMap<String, usize> = HashMap::new();
        let mut reported: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = vec![];
        fn refs<'b>(nodes: &'b [Node]) -> impl Iterator<Item = (&'b Node, &'b str)> { nodes.iter().filter(|n| n.kind == "subgraph").filter_map(|n| n.cfg_str("ref").map(|r| (n, r))) }
        // depth_of(key) = how many sub-graph levels start at `key` (a leaf is 1)
        fn depth_of(v: &mut V, key: &str, memo: &mut HashMap<String, usize>, stack: &mut Vec<String>, reported: &mut HashSet<String>) -> usize {
            if let Some(d) = memo.get(key) { return *d; }
            let doc: &Document = v.doc;
            let Some(s) = doc.subgraph(key) else { return 0 };
            stack.push(key.to_string());
            let mut best = 0;
            for (n, r) in refs(&s.nodes) {
                if let Some(at) = stack.iter().position(|k| k == r) {
                    let chain = stack[at..].iter().cloned().chain(std::iter::once(r.to_string())).collect::<Vec<_>>().join(" -> ");
                    if reported.insert(chain.clone()) {
                        let i = Issue::new(Level::Error, "subgraph.recursive", format!("sub-graph `{key}` contains itself ({chain}) through node `{}`: recursion without a limit is refused", n.id)).node(&n.id);
                        v.push(i, Some(key));
                    }
                    continue;
                }
                best = best.max(depth_of(v, r, memo, stack, reported));
            }
            stack.pop();
            memo.insert(key.to_string(), best + 1);
            best + 1
        }
        for (n, r) in refs(&doc.nodes) {
            let d = depth_of(self, r, &mut memo, &mut stack, &mut reported);
            if d > self.o.max_depth {
                let i = Issue::new(Level::Error, "subgraph.depth", format!("node `{}` opens sub-graphs {d} levels deep; at most {} are allowed", n.id, self.o.max_depth)).node(&n.id);
                self.push(i, None);
            }
        }
        // a sub-graph that no node refers to is only a warning
        let used: BTreeSet<&str> = refs(&doc.nodes).map(|(_, r)| r).chain(doc.subgraphs.iter().flat_map(|m| m.values().flat_map(|s| refs(&s.nodes).map(|(_, r)| r)))).collect();
        for k in doc.subgraphs.iter().flat_map(|m| m.keys()) {
            if !used.contains(k.as_str()) { let i = Issue::new(Level::Warn, "subgraph.unused", format!("sub-graph `{k}` is not used by any node")); self.push(i, Some(k)); }
        }
    }

    fn subgraph_ports(&mut self, key: &str, s: &Subgraph) {
        for (side, decls, kind) in [("input", s.inputs.as_deref(), "input"), ("output", s.outputs.as_deref(), "output")] {
            let Some(decls) = decls else { continue };
            self.decls(decls, &format!("sub-graph `{key}` {side}"), Some(key));
            let inside: Vec<(&Node, String, String)> = s.nodes.iter().filter(|n| n.kind == kind)
                .map(|n| (n, n.cfg_str("name").unwrap_or(&n.id).to_string(), n.cfg_str("type").unwrap_or("json").to_string())).collect();
            for d in decls {
                match inside.iter().find(|(_, n, _)| *n == d.name) {
                    None => { let i = Issue::new(Level::Error, "subgraph.ports", format!("sub-graph `{key}` declares the {side} `{}` but has no `{kind}` node named that inside", d.name)).port(&d.name); self.push(i, Some(key)); }
                    Some((n, _, t)) if PortType::parse(t) != PortType::parse(&d.ty) => {
                        let i = Issue::new(Level::Error, "subgraph.ports", format!("sub-graph `{key}`: the {side} `{}` is declared {} but its `{kind}` node `{}` is {t}", d.name, d.ty, n.id)).node(&n.id).port(&d.name);
                        self.push(i, Some(key));
                    }
                    _ => {}
                }
            }
            for (n, name, _) in &inside {
                if !decls.iter().any(|d| d.name == *name) {
                    let i = Issue::new(Level::Error, "subgraph.ports", format!("sub-graph `{key}` has the `{kind}` node `{}` ({name}) but does not declare an {side} `{name}`", n.id)).node(&n.id).port(name);
                    self.push(i, Some(key));
                }
            }
        }
    }

    /// The ports a `subgraph` node gets from the sub-graph it refers to.
    fn subgraph_node_ports(&self, n: &Node) -> Option<Ports> {
        let s = self.doc.subgraph(n.cfg_str("ref")?)?;
        let from_nodes = |kind: &str| -> Vec<PortDecl> {
            s.nodes.iter().filter(|x| x.kind == kind).map(|x| PortDecl { name: x.cfg_str("name").unwrap_or(&x.id).to_string(), ty: x.cfg_str("type").unwrap_or("json").to_string(), required: None, default: None, extra: Default::default() }).collect()
        };
        let ins = s.inputs.clone().unwrap_or_else(|| from_nodes("input"));
        let outs = s.outputs.clone().unwrap_or_else(|| from_nodes("output"));
        Some(Ports { inputs: decl_ports(&ins, true), outputs: decl_ports(&outs, false) })
    }

    fn ports_of(&self, n: &Node) -> Option<Ports> {
        if n.kind == "subgraph" { self.subgraph_node_ports(n) } else { self.o.registry.ports(n) }
    }

    fn decls(&mut self, ds: &[PortDecl], what: &str, g: Option<&str>) {
        let mut seen = HashSet::new();
        for d in ds {
            if PortType::parse(&d.ty).is_none() { let i = Issue::new(Level::Error, "port.type", format!("{what} `{}` has the unknown type `{}`", d.name, d.ty)).port(&d.name); self.push(i, g); }
            if !seen.insert(&d.name) { let i = Issue::new(Level::Error, "port.duplicate", format!("two {what}s are called `{}`", d.name)).port(&d.name); self.push(i, g); }
        }
    }

    // ----- rules 9 and 10, retry

    fn law_refs(&mut self, ids: Option<&[String]>, whose: &str, node: Option<&str>, g: Option<&str>) {
        let Some(scope) = self.o.scope else { return };
        for id in ids.unwrap_or(&[]) {
            if !scope.law_visible(id) { let mut i = Issue::new(Level::Error, "law.missing", format!("{whose} names the law or group `{id}`, which does not exist or is not visible in this workflow's scope")); i.node = node.map(str::to_string); self.push(i, g); }
        }
    }
    fn prompt_refs(&mut self, ids: Option<&[String]>, whose: &str, node: Option<&str>, g: Option<&str>) {
        let Some(scope) = self.o.scope else { return };
        for id in ids.unwrap_or(&[]) {
            if !scope.prompt_visible(id) { let mut i = Issue::new(Level::Error, "prompt.missing", format!("{whose} names the saved prompt `{id}`, which does not exist or is not visible in this workflow's scope")); i.node = node.map(str::to_string); self.push(i, g); }
        }
    }
    fn model(&mut self, m: &Value, whose: &str, node: Option<&str>, g: Option<&str>) {
        let Some(scope) = self.o.scope else { return };
        let list: Vec<&Value> = std::iter::once(m).chain(m.get("fallback").and_then(Value::as_array).into_iter().flatten()).collect();
        for e in list {
            if !scope.model_reachable(e) {
                let mut i = Issue::new(Level::Warn, "model.unreachable", format!("{whose} ({}) cannot run here: this scope cannot reach it", e.get("model").and_then(Value::as_str).unwrap_or("a model")));
                i.node = node.map(str::to_string);
                self.push(i, g);
            }
        }
    }
    fn retry(&mut self, r: &Value, whose: &str, node: Option<&str>, g: Option<&str>) {
        let ok = r.get("max").and_then(Value::as_u64).is_some_and(|m| m <= 50);
        if !ok { let mut i = Issue::new(Level::Error, "retry.max", format!("{whose} has no maximum: `max` must be a whole number from 0 to 50")); i.node = node.map(str::to_string); self.push(i, g); }
    }

    // ----- one graph level

    fn graph(&mut self, g: G<'a>) {
        let gk = g.key;
        let top = gk.is_none();
        let mut ids: HashSet<&str> = HashSet::new();
        if g.nodes.len() > self.o.max_nodes { let i = Issue::new(Level::Error, "graph.too-big", format!("{} nodes; at most {} are allowed in one graph", g.nodes.len(), self.o.max_nodes)); self.push(i, gk); }
        let mut ports: HashMap<&str, Option<Ports>> = HashMap::new();
        for n in g.nodes {
            if n.id.is_empty() || !ids.insert(&n.id) { let i = Issue::new(Level::Error, "node.duplicate", if n.id.is_empty() { "a node has no id".to_string() } else { format!("two nodes are called `{}`", n.id) }).node(&n.id); self.push(i, gk); }
            ports.insert(&n.id, self.node(n, g));
        }
        // rule 1
        if top {
            let starts: Vec<&Node> = g.nodes.iter().filter(|n| n.kind == "start").collect();
            if starts.is_empty() { let i = Issue::new(Level::Error, "start.missing", "the workflow has no `start` node".to_string()); self.push(i, gk); }
            if starts.len() > 1 { let i = Issue::new(Level::Error, "start.multiple", format!("the workflow has {} `start` nodes ({}); exactly one is allowed", starts.len(), starts.iter().map(|n| format!("`{}`", n.id)).collect::<Vec<_>>().join(", "))).node(&starts[1].id); self.push(i, gk); }
            if !g.nodes.iter().any(|n| n.kind == "end") { let i = Issue::new(Level::Error, "end.missing", "the workflow has no `end` node".to_string()); self.push(i, gk); }
        }
        // rule 2: edges
        let mut fed: HashMap<(&str, &str), Vec<&Edge>> = HashMap::new();
        let mut edge_ids: HashSet<&str> = HashSet::new();
        for e in g.edges {
            if e.id.is_empty() || !edge_ids.insert(&e.id) { let i = Issue::new(Level::Error, "edge.duplicate", format!("two edges are called `{}`", e.id)).edge(&e.id); self.push(i, gk); }
            let (Some((fnode, fport)), Some((tnode, tport))) = (split_endpoint(&e.from), split_endpoint(&e.to)) else {
                let i = Issue::new(Level::Error, "edge.endpoint", format!("edge `{}` must join `<node>.<port>` to `<node>.<port>` (`{}` to `{}`)", e.id, e.from, e.to)).edge(&e.id); self.push(i, gk); continue;
            };
            let mut spec_from = None;
            let mut spec_to = None;
            for (end, nid, pname, out) in [("from", fnode, fport, true), ("to", tnode, tport, false)] {
                let Some(pp) = ports.get(nid) else {
                    let i = Issue::new(Level::Error, "edge.node", format!("edge `{}` {end} `{}`: there is no node `{nid}`", e.id, if out { &e.from } else { &e.to })).edge(&e.id); self.push(i, gk); continue;
                };
                let Some(pp) = pp else { continue };
                let found = if out { pp.output(pname) } else { pp.input(pname) };
                match found {
                    Some(s) => if out { spec_from = Some(s) } else { spec_to = Some(s) },
                    None => {
                        let have: Vec<&str> = (if out { &pp.outputs } else { &pp.inputs }).iter().map(|p| p.name.as_str()).collect();
                        let i = Issue::new(Level::Error, "edge.port", format!("edge `{}`: node `{nid}` has no {} port `{pname}` (it has: {})", e.id, if out { "output" } else { "input" }, if have.is_empty() { "none".to_string() } else { have.join(", ") })).node(nid).port(pname).edge(&e.id);
                        self.push(i, gk);
                    }
                }
            }
            if let (Some(a), Some(b)) = (spec_from, spec_to) {
                if !a.ty.flows_into(&b.ty) {
                    let i = Issue::new(Level::Error, "edge.type", format!("edge `{}` joins {} to {}: a {} cannot go into a {} input", e.id, e.from, e.to, a.ty, b.ty)).node(tnode).port(tport).edge(&e.id); self.push(i, gk);
                }
            }
            if ports.contains_key(tnode) { fed.entry((tnode, tport)).or_default().push(e); }
        }
        // rule 2 (two edges into one input) and rule 3 (required inputs)
        for n in g.nodes {
            let Some(Some(pp)) = ports.get(n.id.as_str()) else { continue };
            for p in &pp.inputs {
                let es = fed.get(&(n.id.as_str(), p.name.as_str())).map(Vec::as_slice).unwrap_or(&[]);
                if es.len() > 1 && !p.many {
                    let i = Issue::new(Level::Error, "input.multiple", format!("input `{}.{}` has {} edges ({}); only a `many` input takes more than one", n.id, p.name, es.len(), es.iter().map(|e| format!("`{}`", e.id)).collect::<Vec<_>>().join(", "))).node(&n.id).port(&p.name).edge(&es[1].id);
                    self.push(i, gk);
                }
                if es.is_empty() && p.required && p.default.is_none() && !has_default(n, &p.name) {
                    let i = Issue::new(Level::Error, "input.required", format!("required input `{}.{}` has no edge and no default", n.id, p.name)).node(&n.id).port(&p.name); self.push(i, gk);
                }
            }
        }
        // rule 4: reachable from start (or, in a sub-graph, from its inputs)
        let root_kind = if top { "start" } else { "input" };
        let roots: Vec<&str> = g.nodes.iter().filter(|n| n.kind == root_kind).map(|n| n.id.as_str()).collect();
        if !roots.is_empty() {
            let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
            for e in g.edges { if let (Some((a, _)), Some((b, _))) = (split_endpoint(&e.from), split_endpoint(&e.to)) { next.entry(a).or_default().push(b); } }
            let mut seen: HashSet<&str> = roots.iter().copied().collect();
            let mut todo = roots.clone();
            while let Some(x) = todo.pop() { for y in next.get(x).into_iter().flatten() { if seen.insert(y) { todo.push(y); } } }
            for n in g.nodes {
                if !seen.contains(n.id.as_str()) && !matches!(n.kind.as_str(), "note" | "comment") && !(n.kind == "start" || n.kind == "input" && !top) {
                    let i = Issue::new(Level::Warn, "node.unreachable", format!("node `{}` ({}) cannot be reached from `{root_kind}`: it will never run", n.id, n.kind)).node(&n.id); self.push(i, gk);
                }
            }
        }
        // rule 5: a cycle must pass through a `loop` node
        self.cycles(g);
        // sections (top level)
        if top {
            let mut sids = HashSet::new();
            for s in self.doc.sections.as_deref().unwrap_or(&[]) {
                if !sids.insert(&s.id) { let i = Issue::new(Level::Error, "section.duplicate", format!("two sections are called `{}`", s.id)); self.push(i, gk); }
                for nid in &s.nodes { if !ids.contains(nid.as_str()) { let i = Issue::new(Level::Error, "section.node", format!("section `{}` contains the node `{nid}`, which does not exist", s.id)).node(nid); self.push(i, gk); } }
                self.law_refs(s.laws.as_deref(), &format!("section `{}`", s.id), None, gk);
                self.prompt_refs(s.prompts.as_deref(), &format!("section `{}`", s.id), None, gk);
                if let Some(m) = &s.model { self.model(m, &format!("section `{}`'s model", s.id), None, gk); }
            }
        }
    }

    /// Per-node rules: 6, 8, 9, 10, templates, conditions. Returns the node's ports (None: unknown).
    fn node(&mut self, n: &'a Node, g: G<'a>) -> Option<Ports> {
        let gk = g.key;
        let reg = self.o.registry;
        if !reg.knows(&n.kind) {
            let i = Issue::new(Level::Error, "kind.unknown", format!("node `{}` has the unknown kind `{}`", n.id, n.kind)).node(&n.id); self.push(i, gk);
        } else {
            let kv = n.kind_version.unwrap_or(1);
            if !reg.has_version(&n.kind, kv) {
                let have = reg.manifest(&n.kind).map(|m| m.version.clone()).unwrap_or_default();
                let i = Issue::new(Level::Error, "kind.version", format!("node `{}` needs version {kv} of the kind `{}`, which is not installed (installed: {have})", n.id, n.kind)).node(&n.id); self.push(i, gk);
            }
            for p in reg.check_config(n) { let i = Issue::new(Level::Error, "config.invalid", format!("node `{}` ({}): {p}", n.id, n.kind)).node(&n.id); self.push(i, gk); }
        }
        if gk.is_some() && n.kind == "start" { let i = Issue::new(Level::Error, "start.in-subgraph", format!("node `{}`: a sub-graph has no `start`; use `input` nodes", n.id)).node(&n.id); self.push(i, gk); }
        if gk.is_none() && (n.kind == "input" || n.kind == "output") { let i = Issue::new(Level::Error, "node.misplaced", format!("node `{}`: `{}` nodes belong inside a sub-graph", n.id, n.kind)).node(&n.id); self.push(i, gk); }
        if n.kind == "subgraph" {
            match n.cfg_str("ref") {
                Some(r) if self.doc.subgraph(r).is_some() => {}
                Some(r) => { let i = Issue::new(Level::Error, "subgraph.missing", format!("node `{}` opens the sub-graph `{r}`, which the document does not have", n.id)).node(&n.id); self.push(i, gk); }
                None => {}
            }
        }
        // rule 6
        match n.kind.as_str() {
            "loop" => self.loop_limit(n, gk),
            "wait" => {
                let has_secs = n.cfg("seconds").is_some_and(Value::is_number);
                if !has_secs && !(n.cfg("until").is_some() && n.cfg("timeoutSec").is_some_and(Value::is_number)) {
                    let i = Issue::new(Level::Error, "wait.timeout", format!("wait `{}` has no timeout: set `seconds`, or `until` with `timeoutSec`", n.id)).node(&n.id); self.push(i, gk);
                }
            }
            "approve" => {
                if !n.cfg("timeoutSec").is_some_and(Value::is_number) && !n.settings.as_ref().and_then(|s| s.get("timeoutSec")).is_some_and(Value::is_number) {
                    let i = Issue::new(Level::Error, "approve.timeout", format!("approve `{}` waits for a person with no timeout: set `timeoutSec`", n.id)).node(&n.id); self.push(i, gk);
                }
            }
            "if" => { if let Some(c) = n.cfg_str("condition") { self.condition(n, c, gk); } }
            _ => {}
        }
        if let Some(s) = &n.settings {
            if let Some(r) = s.get("retry") { self.retry(r, &format!("node `{}`'s retry", n.id), Some(&n.id), gk); }
            if let Some(e) = s.get("onError").and_then(Value::as_str) { if !DEFAULT_ON_ERROR.contains(&e) { let i = Issue::new(Level::Error, "onError.invalid", format!("node `{}`: onError `{e}` must be one of fail, skip, continue-with-default", n.id)).node(&n.id); self.push(i, gk); } }
        }
        // rules 9, 10
        self.law_refs(n.laws.as_deref(), &format!("node `{}`", n.id), Some(&n.id), gk);
        self.prompt_refs(n.prompts.as_deref(), &format!("node `{}`", n.id), Some(&n.id), gk);
        if let Some(m) = n.cfg("model") { self.model(m, &format!("node `{}`'s model", n.id), Some(&n.id), gk); }
        // templates
        if let Some(c) = &n.config { self.templates(n, c, &n.kind, "config", g); }
        self.ports_of(n)
    }

    fn loop_limit(&mut self, n: &Node, gk: Option<&str>) {
        let limit = n.cfg("maxIterations").or_else(|| if n.cfg_str("mode") == Some("count") { n.cfg("count") } else { None });
        match limit {
            None => { let i = Issue::new(Level::Error, "loop.no-limit", format!("loop `{}` has no limit (a loop with no limit): set `maxIterations` from 1 to 100", n.id)).node(&n.id); self.push(i, gk); }
            Some(l) => match l.as_u64() {
                Some(x) if (1..=100).contains(&x) => {}
                _ => { let i = Issue::new(Level::Error, "loop.limit", format!("loop `{}`: the limit {l} must be a whole number from 1 to 100 (the engine's own ceiling)", n.id)).node(&n.id); self.push(i, gk); }
            },
        }
        let mode = n.cfg_str("mode").unwrap_or("while");
        if mode != "count" {
            match n.cfg_str("condition") {
                None => { let i = Issue::new(Level::Error, "loop.condition", format!("loop `{}` (mode {mode}) needs a `condition`", n.id)).node(&n.id); self.push(i, gk); }
                Some(c) => self.condition(n, c, gk),
            }
        }
    }

    fn condition(&mut self, n: &Node, c: &str, gk: Option<&str>) {
        if let Err(e) = expr::parse_condition(c) { let i = Issue::new(Level::Error, "condition.syntax", format!("node `{}`: the condition `{c}` does not parse: {e}", n.id)).node(&n.id); self.push(i, gk); }
    }

    fn templates(&mut self, n: &Node, v: &Value, kind: &str, path: &str, g: G<'a>) {
        match v {
            Value::String(s) if s.contains("{{") => self.template(n, s, path, g),
            Value::Array(a) => for (i, x) in a.iter().enumerate() { self.templates(n, x, kind, &format!("{path}[{i}]"), g); },
            Value::Object(o) => for (k, x) in o { if !(kind == "script" && k == "code") { self.templates(n, x, kind, &format!("{path}.{k}"), g); } },
            _ => {}
        }
    }

    fn template(&mut self, n: &Node, s: &str, path: &str, g: G<'a>) {
        let gk = g.key;
        let t = match expr::parse_template(s) {
            Ok(t) => t,
            Err(e) => { let i = Issue::new(Level::Error, "template.syntax", format!("node `{}`, `{path}`: {e}", n.id)).node(&n.id); self.push(i, gk); return; }
        };
        for name in t.prompt_refs() {
            if let Some(sc) = self.o.scope { if !sc.prompt_visible(name) { let i = Issue::new(Level::Error, "prompt.missing", format!("node `{}`, `{path}` uses the saved prompt `{name}`, which does not exist or is not visible in this workflow's scope", n.id)).node(&n.id); self.push(i, gk); } }
        }
        for p in t.paths() {
            match p[0].as_str() {
                "node" => {
                    let Some(target) = p.get(1) else { continue };
                    let Some(tn) = g.nodes.iter().find(|x| x.id == *target) else {
                        let i = Issue::new(Level::Error, "template.node", format!("node `{}`, `{path}`: `{{{{{}}}}}` names the node `{target}`, which does not exist", n.id, p.join("."))).node(&n.id); self.push(i, gk); continue;
                    };
                    if p.get(2).map(String::as_str) == Some("out") {
                        if let (Some(port), Some(pp)) = (p.get(3), self.ports_of(tn)) {
                            if pp.output(port).is_none() { let i = Issue::new(Level::Error, "template.port", format!("node `{}`, `{path}`: `{{{{{}}}}}`: node `{target}` has no output `{port}`", n.id, p.join("."))).node(&n.id).port(port); self.push(i, gk); }
                        }
                    }
                }
                "inputs" if gk.is_none() => {
                    if let (Some(name), Some(decls)) = (p.get(1), &self.doc.inputs) {
                        if name != "prompt" && !decls.iter().any(|d| d.name == *name) { let i = Issue::new(Level::Warn, "template.input", format!("node `{}`, `{path}`: `{{{{{}}}}}` reads a workflow input `{name}` that is not declared", n.id, p.join("."))).node(&n.id); self.push(i, gk); }
                    }
                }
                "inputs" | "state" | "loop" | "item" => {}
                other => { let i = Issue::new(Level::Warn, "template.root", format!("node `{}`, `{path}`: `{{{{{}}}}}` starts with `{other}`; values come from `inputs`, `node` or `state`", n.id, p.join("."))).node(&n.id); self.push(i, gk); }
            }
        }
    }

    fn cycles(&mut self, g: G<'a>) {
        let ix: HashMap<&str, usize> = g.nodes.iter().enumerate().filter(|(_, n)| n.kind != "loop").map(|(i, n)| (n.id.as_str(), i)).collect();
        let mut adj: Vec<Vec<usize>> = vec![vec![]; g.nodes.len()];
        let mut selfloop = vec![false; g.nodes.len()];
        for e in g.edges {
            if let (Some((a, _)), Some((b, _))) = (split_endpoint(&e.from), split_endpoint(&e.to)) {
                if let (Some(&x), Some(&y)) = (ix.get(a), ix.get(b)) { adj[x].push(y); if x == y { selfloop[x] = true; } }
            }
        }
        // Tarjan's strongly connected components, on the graph without its loop nodes: a cycle left is unbounded
        struct T<'b> { adj: &'b [Vec<usize>], idx: Vec<Option<usize>>, low: Vec<usize>, on: Vec<bool>, st: Vec<usize>, next: usize, sccs: Vec<Vec<usize>> }
        fn go(t: &mut T, v: usize) {
            t.idx[v] = Some(t.next); t.low[v] = t.next; t.next += 1; t.st.push(v); t.on[v] = true;
            for k in 0..t.adj[v].len() {
                let w = t.adj[v][k];
                match t.idx[w] {
                    None => { go(t, w); t.low[v] = t.low[v].min(t.low[w]); }
                    Some(iw) if t.on[w] => t.low[v] = t.low[v].min(iw),
                    _ => {}
                }
            }
            if Some(t.low[v]) == t.idx[v] {
                let mut c = vec![];
                loop { let w = t.st.pop().unwrap(); t.on[w] = false; c.push(w); if w == v { break; } }
                t.sccs.push(c);
            }
        }
        let mut t = T { adj: &adj, idx: vec![None; g.nodes.len()], low: vec![0; g.nodes.len()], on: vec![false; g.nodes.len()], st: vec![], next: 0, sccs: vec![] };
        for &v in ix.values().collect::<BTreeSet<_>>() { if t.idx[v].is_none() { go(&mut t, v); } }
        let mut found: Vec<Vec<usize>> = t.sccs.into_iter().filter(|c| c.len() > 1 || selfloop[c[0]]).collect();
        for c in &mut found { c.sort(); }
        found.sort();
        for c in found {
            let names = c.iter().map(|&i| format!("`{}`", g.nodes[i].id)).collect::<Vec<_>>().join(", ");
            let i = Issue::new(Level::Error, "graph.cycle", format!("{names} form a cycle that does not pass through a `loop` node (a loop with no limit): a wire going backwards must go through a `loop`")).node(&g.nodes[c[0]].id);
            self.push(i, g.key);
        }
    }
}

/// A value in the node's config can stand in for an unfed input: `config.<port>`, `config.defaults.<port>` or
/// `config.inputs.<port>` (spec 3.1).
fn has_default(n: &Node, port: &str) -> bool {
    let set = |v: Option<&Value>| v.is_some_and(|x| !x.is_null());
    set(n.cfg(port)) || set(n.cfg("defaults").and_then(|d| d.get(port))) || set(n.cfg("inputs").and_then(|d| d.get(port)))
}
