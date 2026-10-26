//! The workflow document (spec section 13).
//!
//! Every struct keeps the fields it does not know in `extra` and writes them back after its own, so a newer editor's
//! fields survive an older one. Optional fields are `Option` (absent stays absent, `[]` stays `[]`), and the fields are
//! declared in the order of the spec's example, which is the canonical order a write gives.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;

type Extra = Map<String, Value>;

fn is_empty_map(m: &Extra) -> bool { m.is_empty() }

/// A workflow input / output (also a sub-graph's): `{ "name": "audience", "type": "text", "default": "team" }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortDecl {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

/// One box on the canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: String,
    #[serde(rename = "kindVersion", default, skip_serializing_if = "Option::is_none")]
    pub kind_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub laws: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Vec<Number>>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

impl Node {
    pub fn new(id: &str, kind: &str) -> Node {
        Node { id: id.to_string(), kind: kind.to_string(), kind_version: Some(1), label: None, config: None, laws: None, prompts: None, settings: None, pos: None, extra: Map::new() }
    }
    pub fn with_config(mut self, config: Value) -> Node { self.config = Some(config); self }
    /// A config value by key (None when there is no config or no such key).
    pub fn cfg(&self, key: &str) -> Option<&Value> { self.config.as_ref().and_then(|c| c.get(key)) }
    pub fn cfg_str(&self, key: &str) -> Option<&str> { self.cfg(key).and_then(Value::as_str) }
}

/// A wire: `from` / `to` are `"<node id>.<port name>"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

impl Edge {
    pub fn new(id: &str, from: &str, to: &str) -> Edge { Edge { id: id.to_string(), from: from.to_string(), to: to.to_string(), extra: Map::new() } }
}

/// A frame drawn around nodes: a label, a colour and settings for everything inside. Not a sub-graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub nodes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub laws: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Vec<Number>>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

/// The inside of a `subgraph` node: the same shape as the top level, with `input` and `output` nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subgraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<PortDecl>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<PortDecl>>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

/// What every node inherits unless its section or itself says otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub laws: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<Value>,
    #[serde(rename = "onError", default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

/// A workflow document. See the spec's section 13 for the fields; this is that shape, field for field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub schema: u32,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(rename = "runOn", default, skip_serializing_if = "Option::is_none")]
    pub run_on: Option<String>,
    #[serde(rename = "needsProject", default, skip_serializing_if = "Option::is_none")]
    pub needs_project: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<PortDecl>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<PortDecl>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<Defaults>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<Section>>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgraphs: Option<BTreeMap<String, Subgraph>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perms: Option<Value>,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    #[serde(rename = "publishedAt", default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<u64>,
    #[serde(rename = "publishedBy", default, skip_serializing_if = "Option::is_none")]
    pub published_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

impl Document {
    /// A new, empty document (schema [`crate::SCHEMA`]): no nodes yet.
    pub fn new(id: &str, name: &str) -> Document {
        Document {
            schema: crate::SCHEMA, id: id.to_string(), name: name.to_string(), title: None, description: None, scope: None, owner: None, version: None,
            labels: None, run_on: None, needs_project: None, inputs: None, outputs: None, defaults: None, sections: None, nodes: vec![], edges: vec![],
            subgraphs: None, perms: None, created_at: None, updated_at: None, published_at: None, published_by: None, message: None, extra: Map::new(),
        }
    }
    pub fn node(&self, id: &str) -> Option<&Node> { self.nodes.iter().find(|n| n.id == id) }
    pub fn edge(&self, id: &str) -> Option<&Edge> { self.edges.iter().find(|e| e.id == id) }
    pub fn subgraph(&self, key: &str) -> Option<&Subgraph> { self.subgraphs.as_ref().and_then(|m| m.get(key)) }
}
