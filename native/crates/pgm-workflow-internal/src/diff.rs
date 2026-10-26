//! `diff(v1, v2)`: two versions of a workflow compared node by node (spec 9.1).
//!
//! Nodes and edges are matched by id (ids are stable and never reused). A node that is in both is *changed* when its
//! kind, one of its config keys (top level of `config`) or another field changed; moving it on the canvas (`pos`) is
//! not a change. An edge in both whose `from` or `to` differs is *rewired*. Sub-graphs are compared the same way, key
//! by key. `meta` names the document's own fields that changed (title, defaults, sections, ...).
use crate::model::{Document, Edge, Node};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigChange {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeChange {
    pub id: String,
    /// (before, after) when the node's kind changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<(String, String)>,
    pub config: Vec<ConfigChange>,
    /// Other fields that changed (`label`, `laws`, `settings`, ...).
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rewire {
    pub edge: String,
    pub before: (String, String),
    pub after: (String, String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GraphDiff {
    pub added_nodes: Vec<String>,
    pub removed_nodes: Vec<String>,
    pub changed_nodes: Vec<NodeChange>,
    pub added_edges: Vec<String>,
    pub removed_edges: Vec<String>,
    pub rewired_edges: Vec<Rewire>,
}

impl GraphDiff {
    pub fn is_empty(&self) -> bool { *self == GraphDiff::default() }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DocDiff {
    #[serde(flatten)]
    pub graph: GraphDiff,
    pub added_subgraphs: Vec<String>,
    pub removed_subgraphs: Vec<String>,
    pub subgraphs: BTreeMap<String, GraphDiff>,
    pub meta: Vec<String>,
}

impl DocDiff {
    pub fn is_empty(&self) -> bool { *self == DocDiff::default() }
    pub fn to_json(&self) -> Value { serde_json::to_value(self).unwrap_or(Value::Null) }

    /// One line per difference, for a person.
    pub fn summary(&self) -> Vec<String> {
        let mut out = vec![];
        let mut lines = |g: &GraphDiff, p: &str| {
            for n in &g.added_nodes { out.push(format!("{p}added node {n}")); }
            for n in &g.removed_nodes { out.push(format!("{p}removed node {n}")); }
            for c in &g.changed_nodes {
                if let Some((a, b)) = &c.kind { out.push(format!("{p}node {} changed kind {a} -> {b}", c.id)); }
                for k in &c.config { out.push(format!("{p}node {} changed config `{}`", c.id, k.key)); }
                for f in &c.fields { out.push(format!("{p}node {} changed {f}", c.id)); }
            }
            for e in &g.added_edges { out.push(format!("{p}added edge {e}")); }
            for e in &g.removed_edges { out.push(format!("{p}removed edge {e}")); }
            for r in &g.rewired_edges { out.push(format!("{p}rewired edge {}: {} -> {} became {} -> {}", r.edge, r.before.0, r.before.1, r.after.0, r.after.1)); }
        };
        lines(&self.graph, "");
        for (k, g) in &self.subgraphs { lines(g, &format!("sub-graph {k}: ")); }
        for k in &self.added_subgraphs { out.push(format!("added sub-graph {k}")); }
        for k in &self.removed_subgraphs { out.push(format!("removed sub-graph {k}")); }
        for m in &self.meta { out.push(format!("changed {m}")); }
        out
    }
}

/// What changed from `a` (the older) to `b` (the newer).
pub fn diff(a: &Document, b: &Document) -> DocDiff {
    let mut d = DocDiff { graph: graph_diff(&a.nodes, &a.edges, &b.nodes, &b.edges), ..Default::default() };
    let (sa, sb) = (a.subgraphs.clone().unwrap_or_default(), b.subgraphs.clone().unwrap_or_default());
    for k in sb.keys() { if !sa.contains_key(k) { d.added_subgraphs.push(k.clone()); } }
    for (k, x) in &sa {
        match sb.get(k) {
            None => d.removed_subgraphs.push(k.clone()),
            Some(y) => { let g = graph_diff(&x.nodes, &x.edges, &y.nodes, &y.edges); if !g.is_empty() { d.subgraphs.insert(k.clone(), g); } }
        }
    }
    let skip = ["schema", "nodes", "edges", "subgraphs", "version", "createdAt", "updatedAt", "publishedAt", "publishedBy", "message"];
    let (va, vb) = (crate::doc::to_value(a), crate::doc::to_value(b));
    let (oa, ob) = (va.as_object().cloned().unwrap_or_default(), vb.as_object().cloned().unwrap_or_default());
    let keys: BTreeSet<&String> = oa.keys().chain(ob.keys()).filter(|k| !skip.contains(&k.as_str())).collect();
    for k in keys { if oa.get(k) != ob.get(k) { d.meta.push(k.clone()); } }
    d
}

fn graph_diff(an: &[Node], ae: &[Edge], bn: &[Node], be: &[Edge]) -> GraphDiff {
    let mut g = GraphDiff::default();
    for n in bn { if !an.iter().any(|x| x.id == n.id) { g.added_nodes.push(n.id.clone()); } }
    for x in an {
        match bn.iter().find(|n| n.id == x.id) {
            None => g.removed_nodes.push(x.id.clone()),
            Some(y) => { if let Some(c) = node_change(x, y) { g.changed_nodes.push(c); } }
        }
    }
    for e in be { if !ae.iter().any(|x| x.id == e.id) { g.added_edges.push(e.id.clone()); } }
    for x in ae {
        match be.iter().find(|e| e.id == x.id) {
            None => g.removed_edges.push(x.id.clone()),
            Some(y) if x.from != y.from || x.to != y.to => g.rewired_edges.push(Rewire { edge: x.id.clone(), before: (x.from.clone(), x.to.clone()), after: (y.from.clone(), y.to.clone()) }),
            _ => {}
        }
    }
    g
}

fn node_change(a: &Node, b: &Node) -> Option<NodeChange> {
    let mut c = NodeChange { id: a.id.clone(), kind: (a.kind != b.kind).then(|| (a.kind.clone(), b.kind.clone())), config: vec![], fields: vec![] };
    let obj = |n: &Node| n.config.as_ref().and_then(Value::as_object).cloned().unwrap_or_default();
    let (ca, cb) = (obj(a), obj(b));
    let keys: BTreeSet<&String> = ca.keys().chain(cb.keys()).collect();
    for k in keys { if ca.get(k) != cb.get(k) { c.config.push(ConfigChange { key: k.clone(), before: ca.get(k).cloned(), after: cb.get(k).cloned() }); } }
    // a config that is not an object at all (rare) is compared whole
    if a.config.as_ref().is_some_and(|v| !v.is_object()) || b.config.as_ref().is_some_and(|v| !v.is_object()) { if a.config != b.config { c.fields.push("config".into()); } }
    let rest = |n: &Node| { let mut v = serde_json::to_value(n).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default(); for k in ["id", "kind", "config", "pos"] { v.remove(k); } v };
    let (ra, rb) = (rest(a), rest(b));
    let keys: BTreeSet<&String> = ra.keys().chain(rb.keys()).collect();
    for k in keys { if ra.get(k) != rb.get(k) { c.fields.push(k.clone()); } }
    if c.kind.is_none() && c.config.is_empty() && c.fields.is_empty() { None } else { Some(c) }
}
