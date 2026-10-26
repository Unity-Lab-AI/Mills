//! Node kinds: manifests (spec 11.1), the registry trait validation asks, and the built-in kinds.
//!
//! A **manifest** says what a kind is: its name, version, ports, and a JSON schema for its config. A **registry**
//! answers "is this kind known, is this version installed, what are this node's ports, does its config fit".
//! [`DefaultRegistry`] knows the built-in kinds' port signatures (the specials of spec 3.2 and the kinds of 5.x) and
//! takes more through [`DefaultRegistry::register`] (custom nodes, section 11). The engine's behaviour for a kind is
//! not here (T-201): only its shape.
use crate::model::Node;
use crate::types::{PortSpec, PortType, Ports};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

type Extra = Map<String, Value>;
fn is_empty_map(m: &Extra) -> bool { m.is_empty() }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestPort {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub many: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManifestPorts {
    #[serde(rename = "in", default)]
    pub inputs: Vec<ManifestPort>,
    #[serde(rename = "out", default)]
    pub outputs: Vec<ManifestPort>,
}

/// The package manifest of a node kind (spec 11.1). Unknown fields are kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindManifest {
    pub schema: u32,
    pub kind: String,
    /// `"1.2.0"`: a document keeps the kind's major (`kindVersion`); the engine runs it or a newer same-major version.
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub ports: ManifestPorts,
    /// A JSON schema (the subset [`schema_check`] knows): the form the editor draws, and what validation checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    #[serde(rename = "impl", default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

/// `"1.2.0"` -> (1, 2, 0); a missing minor / patch is 0.
pub fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.trim().split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().map_or(Some(0), |x| x.parse().ok())?;
    let patch = it.next().map_or(Some(0), |x| x.parse().ok())?;
    if it.next().is_some() { None } else { Some((major, minor, patch)) }
}

fn to_spec(p: &ManifestPort) -> Result<PortSpec, String> {
    let ty = PortType::parse(&p.ty).ok_or_else(|| format!("port `{}` has the unknown type `{}`", p.name, p.ty))?;
    Ok(PortSpec { name: p.name.clone(), ty, required: p.required.unwrap_or(false), many: p.many.unwrap_or(false), default: p.default.clone() })
}

impl KindManifest {
    /// The major version (0 when the version text is not a version).
    pub fn major(&self) -> u32 { parse_semver(&self.version).map_or(0, |v| v.0) }

    /// The fixed ports as the type rules use them.
    pub fn port_specs(&self) -> Result<Ports, String> {
        Ok(Ports { inputs: self.ports.inputs.iter().map(to_spec).collect::<Result<_, _>>()?, outputs: self.ports.outputs.iter().map(to_spec).collect::<Result<_, _>>()? })
    }

    /// What is wrong with the manifest itself (empty: fine).
    pub fn problems(&self) -> Vec<String> {
        let mut out = vec![];
        if self.kind.is_empty() || !self.kind.chars().all(|c| c.is_ascii_alphanumeric() || "-_/.".contains(c)) { out.push(format!("the kind name `{}` may only hold letters, digits and - _ / .", self.kind)); }
        if parse_semver(&self.version).is_none() { out.push(format!("the version `{}` is not major.minor.patch", self.version)); }
        for (side, ports) in [("in", &self.ports.inputs), ("out", &self.ports.outputs)] {
            let mut seen = vec![];
            for p in ports {
                if let Err(e) = to_spec(p) { out.push(e); }
                if seen.contains(&&p.name) { out.push(format!("two `{side}` ports are called `{}`", p.name)); }
                seen.push(&p.name);
            }
        }
        out
    }
}

/// What validation asks about node kinds. Implement it to add kinds the library does not know (the engine's own
/// registry of custom nodes); or wrap / extend [`DefaultRegistry`].
pub trait KindRegistry {
    /// The kind's manifest, None when the kind is not known (an error in the document, rule 8).
    fn manifest(&self, kind: &str) -> Option<&KindManifest>;
    fn knows(&self, kind: &str) -> bool { self.manifest(kind).is_some() }
    /// Is this version (the document's `kindVersion`, a major) installed?
    fn has_version(&self, kind: &str, kind_version: u32) -> bool { self.manifest(kind).is_some_and(|m| m.major() == kind_version) }
    /// The node's ports: the kind's own, plus any its config adds. None when the kind is unknown.
    fn ports(&self, node: &Node) -> Option<Ports> { self.manifest(&node.kind)?.port_specs().ok() }
    /// What is wrong with the node's config against the kind's schema (empty: fits).
    fn check_config(&self, node: &Node) -> Vec<String> {
        match (self.manifest(&node.kind).and_then(|m| m.config.as_ref()), node.config.as_ref()) {
            (Some(schema), Some(cfg)) => { let mut out = vec![]; schema_check(schema, cfg, "config", &mut out); out }
            (Some(schema), None) => { let mut out = vec![]; schema_check(schema, &json!({}), "config", &mut out); out }
            _ => vec![],
        }
    }
}

/// A small JSON-schema subset: `type` (string or list; `integer` too), `enum`, `required`, `properties`,
/// `additionalProperties: false`, `items`, `minimum`, `maximum`. Anything else in a schema is ignored.
pub fn schema_check(schema: &Value, v: &Value, path: &str, out: &mut Vec<String>) {
    let Some(s) = schema.as_object() else { return };
    if let Some(t) = s.get("type") {
        let types: Vec<&str> = match t { Value::String(x) => vec![x.as_str()], Value::Array(a) => a.iter().filter_map(Value::as_str).collect(), _ => vec![] };
        let ok = types.is_empty() || types.iter().any(|t| match *t {
            "object" => v.is_object(), "array" => v.is_array(), "string" => v.is_string(), "boolean" => v.is_boolean(), "null" => v.is_null(),
            "number" => v.is_number(), "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0), _ => true,
        });
        if !ok { out.push(format!("`{path}` must be {}", types.join(" or "))); return; }
    }
    if let Some(Value::Array(e)) = s.get("enum") {
        if !e.contains(v) { out.push(format!("`{path}` must be one of {}", e.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "))); }
    }
    if let (Some(n), Some(min)) = (v.as_f64(), s.get("minimum").and_then(Value::as_f64)) { if n < min { out.push(format!("`{path}` must be at least {min}")); } }
    if let (Some(n), Some(max)) = (v.as_f64(), s.get("maximum").and_then(Value::as_f64)) { if n > max { out.push(format!("`{path}` must be at most {max}")); } }
    if let Some(o) = v.as_object() {
        if let Some(Value::Array(req)) = s.get("required") { for r in req.iter().filter_map(Value::as_str) { if !o.contains_key(r) { out.push(format!("`{path}.{r}` is required")); } } }
        let props = s.get("properties").and_then(Value::as_object);
        if let Some(props) = props { for (k, sub) in props { if let Some(x) = o.get(k) { schema_check(sub, x, &format!("{path}.{k}"), out); } } }
        if s.get("additionalProperties") == Some(&Value::Bool(false)) { for k in o.keys() { if !props.is_some_and(|p| p.contains_key(k)) { out.push(format!("`{path}.{k}` is not a setting of this kind")); } } }
    }
    if let (Some(a), Some(item)) = (v.as_array(), s.get("items")) { for (i, x) in a.iter().enumerate() { schema_check(item, x, &format!("{path}[{i}]"), out); } }
}

// ---------------------------------------------------------------- the built-in kinds

fn p(name: &str, ty: &str) -> PortSpec { PortSpec::new(name, PortType::parse(ty).expect("built-in port type")) }

fn mport(s: &PortSpec) -> ManifestPort {
    ManifestPort { name: s.name.clone(), ty: s.ty.to_string(), required: s.required.then_some(true), many: s.many.then_some(true), default: s.default.clone(), extra: Map::new() }
}

fn kind(kind: &str, category: &str, title: &str, ins: Vec<PortSpec>, outs: Vec<PortSpec>, config: Value) -> KindManifest {
    KindManifest {
        schema: 1, kind: kind.to_string(), version: "1.0.0".to_string(), title: Some(title.to_string()), category: Some(category.to_string()), icon: None,
        ports: ManifestPorts { inputs: ins.iter().map(mport).collect(), outputs: outs.iter().map(mport).collect() }, config: Some(config),
        implementation: Some(json!({ "type": "builtin" })), permissions: None, idempotent: None, sensitive: None, tests: None, docs: None, extra: Map::new(),
    }
}

fn builtins() -> Vec<KindManifest> {
    let err = || p("error", "error");
    let any = json!({ "type": "object" });
    vec![
        // the specials (3.2)
        kind("start", "Flow", "Start", vec![], vec![p("prompt", "text"), p("inputs", "json"), p("files", "list<file>"), p("sender", "text"), p("project", "text")], any.clone()),
        kind("end", "Flow", "End", vec![p("result", "json").many()], vec![], json!({ "type": "object", "properties": { "name": { "type": "string" } } })),
        kind("input", "Flow", "Sub-graph input", vec![], vec![p("value", "json")], json!({ "type": "object", "properties": { "name": { "type": "string" }, "type": { "type": "string" } } })),
        kind("output", "Flow", "Sub-graph output", vec![p("value", "json").required()], vec![], json!({ "type": "object", "properties": { "name": { "type": "string" }, "type": { "type": "string" } } })),
        kind("note", "Flow", "Note", vec![], vec![], any.clone()),
        kind("comment", "Flow", "Comment", vec![], vec![], any.clone()),
        // sub-graphs (ports come from the document's `subgraphs`, see validate.rs)
        kind("subgraph", "Flow", "Sub-graph", vec![], vec![], json!({ "type": "object", "required": ["ref"], "properties": { "ref": { "type": "string" } } })),
        // AI nodes (5.1)
        kind("prompt", "AI", "Prompt", vec![p("in", "json").required()], vec![p("text", "text"), p("json", "json"), p("usage", "json"), err()],
            json!({ "type": "object", "properties": { "system": { "type": "string" }, "user": { "type": "string" }, "model": { "type": "object" }, "temperature": { "type": "number", "minimum": 0 },
                "maxTokens": { "type": "integer", "minimum": 1 }, "responseFormat": { "enum": ["text", "json", "json-schema"] }, "stream": { "type": "boolean" } } })),
        kind("chat", "AI", "Chat", vec![p("in", "json").required()], vec![p("messages", "messages"), p("last", "text"), p("turns", "number"), p("stopped", "text"), err()],
            json!({ "type": "object", "properties": { "system": { "type": "string" }, "opening": { "type": "string" }, "maxTurns": { "type": "integer", "minimum": 1 }, "stopWhen": { "type": "string" } } })),
        kind("summarise", "AI", "Summarise", vec![p("text", "text").required()], vec![p("summary", "text"), p("kept", "list<text>"), err()],
            json!({ "type": "object", "properties": { "target": { "type": "object" }, "style": { "type": "string" }, "keep": { "type": "array", "items": { "type": "string" } } } })),
        kind("jev", "AI", "JEV decision", vec![p("context", "json").required(), p("question", "text")],
            vec![p("choice", "text"), p("reason", "text"), p("confidence", "number"), err()],
            json!({ "type": "object", "properties": { "question": { "type": "string" }, "mode": { "enum": ["pick", "yes-no", "score", "rank"] }, "model": { "type": "object" },
                "choices": { "type": "array" }, "minConfidence": { "type": "number", "minimum": 0, "maximum": 1 },
                "onLowConfidence": { "enum": ["ask-person", "second-opinion", "default", "error"] }, "votes": { "type": "integer", "minimum": 1 } } })),
        // tools and scripts (5.2, 5.3)
        kind("tool", "Tools", "Tool call", vec![p("args", "json")], vec![p("result", "json"), err()], json!({ "type": "object", "properties": { "tool": { "type": "string" } } })),
        kind("script", "Tools", "Script", vec![p("in", "json")], vec![err()],
            json!({ "type": "object", "properties": { "lang": { "type": "string" }, "code": { "type": "string" }, "outputs": { "type": "array" } } })),
        // reverse engineering (T-255): the host's standalone RE tools (re_*), one throw-away copy per node unless kept
        kind("rea", "Tools", "Reverse engineering (REA)", vec![p("args", "json"), p("copy", "text")],
            vec![p("result", "json"), p("text", "text"), p("output", "json"), p("files", "json"), p("copy", "text"), err()],
            json!({ "type": "object", "properties": { "tool": { "type": "string" }, "args": { "type": "object" }, "exec": { "type": ["string", "array"] },
                "files": { "type": "array", "items": { "type": "object", "required": ["path"], "properties": { "path": { "type": "string" }, "text": { "type": "string" }, "base64": { "type": "string" } } } },
                "get": { "type": "array", "items": { "type": "string" } }, "purpose": { "type": "string" }, "copy": { "type": "string" }, "keep": { "type": "boolean" }, "stop": { "type": "boolean" },
                "timeoutSec": { "type": "integer", "minimum": 1 }, "queueSec": { "type": "integer", "minimum": 0 }, "failOnExit": { "type": "boolean" } }, "additionalProperties": false })),
        // agents and orchestration (5.5)
        kind("agent", "Agents", "Agent", vec![p("task", "text").required()], vec![p("result", "json"), p("text", "text"), err()], json!({ "type": "object", "properties": { "timeoutSec": { "type": "number", "minimum": 1 } } })),
        kind("orchestrate", "Agents", "Orchestrate", vec![p("goal", "text").required()], vec![p("result", "json"), err()], any.clone()),
        kind("sub-workflow", "Agents", "Sub-workflow", vec![p("in", "json")], vec![p("result", "json"), p("outcome", "text"), err()],
            json!({ "type": "object", "required": ["name"], "properties": { "name": { "type": "string" }, "range": { "type": "string" }, "inputs": { "type": "object" } } })),
        // people (4.6)
        kind("approve", "People", "Approve", vec![p("in", "json").required()], vec![p("approved", "flow"), p("rejected", "flow"), p("answer", "json"), p("text", "text"), err()],
            json!({ "type": "object", "properties": { "who": { "type": "string" }, "timeoutSec": { "type": "number", "minimum": 1 }, "onTimeout": { "type": "string" }, "questions": { "type": "array" } } })),
        // flow control (4.2 - 4.4)
        kind("if", "Flow", "If", vec![p("value", "json").required()], vec![p("true", "flow"), p("false", "flow")], json!({ "type": "object", "properties": { "condition": { "type": "string" } } })),
        kind("switch", "Flow", "Switch", vec![p("value", "json").required()], vec![p("default", "flow")], json!({ "type": "object", "properties": { "cases": { "type": "array" } } })),
        kind("parallel", "Flow", "Parallel", vec![p("in", "json").required()], vec![p("out", "json")], json!({ "type": "object", "properties": { "mode": { "enum": ["copies", "split", "fixed"] }, "n": { "type": "integer", "minimum": 1 }, "lines": { "type": "array" } } })),
        kind("join", "Flow", "Join", vec![p("lines", "json").required().many()], vec![p("result", "json"), err()],
            json!({ "type": "object", "properties": { "mode": { "enum": ["all", "any", "first", "n-of-m", "merge"] }, "onFail": { "enum": ["fail", "skip", "collect"] } } })),
        kind("for-each", "Flow", "For each", vec![p("items", "list<json>").required()], vec![p("results", "list<json>"), err()], json!({ "type": "object", "properties": { "ref": { "type": "string" } } })),
        kind("loop", "Flow", "Loop", vec![p("in", "json").required(), p("back", "json").many()],
            vec![p("again", "json"), p("done", "json"), p("limit", "json"), p("round", "number"), err()],
            json!({ "type": "object", "properties": { "mode": { "enum": ["while", "until", "count"] }, "condition": { "type": "string" }, "count": { "type": "integer", "minimum": 1 },
                "maxIterations": { "type": "integer", "minimum": 1, "maximum": 100 }, "maxSeconds": { "type": "number", "minimum": 1 }, "maxCost": { "type": "number", "minimum": 0 } } })),
        kind("wait", "Flow", "Wait", vec![p("in", "json")], vec![p("out", "json"), err()], json!({ "type": "object", "properties": { "seconds": { "type": "number", "minimum": 0 }, "until": { "type": "string" }, "timeoutSec": { "type": "number", "minimum": 1 } } })),
        kind("set", "Flow", "Set state", vec![p("value", "json").required()], vec![p("out", "json")], json!({ "type": "object", "properties": { "key": { "type": "string" } } })),
        kind("emit", "Flow", "Emit", vec![p("message", "text").required()], vec![p("done", "flow")], any),
    ]
}

/// The built-in kinds, plus whatever is registered.
#[derive(Debug, Clone)]
pub struct DefaultRegistry { kinds: BTreeMap<String, KindManifest> }

impl Default for DefaultRegistry { fn default() -> Self { DefaultRegistry::new() } }

impl DefaultRegistry {
    pub fn new() -> Self { DefaultRegistry { kinds: builtins().into_iter().map(|m| (m.kind.clone(), m)).collect() } }
    /// No built-ins at all (to prove a registry is asked, in tests; or for a registry made of manifests alone).
    pub fn empty() -> Self { DefaultRegistry { kinds: BTreeMap::new() } }
    /// Add a kind (a custom node). Refused when its manifest has problems or when it would replace a built-in.
    pub fn register(&mut self, m: KindManifest) -> Result<(), Vec<String>> {
        let mut problems = m.problems();
        if builtins().iter().any(|b| b.kind == m.kind) { problems.push(format!("`{}` is a built-in kind and cannot be replaced", m.kind)); }
        if !problems.is_empty() { return Err(problems); }
        self.kinds.insert(m.kind.clone(), m);
        Ok(())
    }
    pub fn kinds(&self) -> impl Iterator<Item = &KindManifest> { self.kinds.values() }
}

fn names(v: Option<&Value>, key_of_obj: &[&str]) -> Vec<(String, Option<String>)> {
    let Some(Value::Array(a)) = v else { return vec![] };
    a.iter().enumerate().map(|(i, x)| match x {
        Value::String(s) => (s.clone(), None),
        Value::Object(o) => (key_of_obj.iter().find_map(|k| o.get(*k).and_then(Value::as_str)).map(str::to_string).unwrap_or_else(|| format!("case{}", i + 1)), o.get("type").and_then(Value::as_str).map(str::to_string)),
        _ => (format!("case{}", i + 1), None),
    }).collect()
}

impl KindRegistry for DefaultRegistry {
    fn manifest(&self, kind: &str) -> Option<&KindManifest> { self.kinds.get(kind) }

    fn ports(&self, node: &Node) -> Option<Ports> {
        let mut ports = self.kinds.get(&node.kind)?.port_specs().ok()?;
        let ty_of = |n: &Node| n.cfg_str("type").and_then(PortType::parse).unwrap_or(PortType::Json);
        match node.kind.as_str() {
            "input" => ports.outputs[0].ty = ty_of(node),
            "output" => ports.inputs[0].ty = ty_of(node),
            "script" => {
                let outs = names(node.cfg("outputs"), &["name"]);
                let outs = if outs.is_empty() { vec![("out".to_string(), None)] } else { outs };
                for (i, (n, t)) in outs.into_iter().enumerate() { ports.outputs.insert(i, PortSpec::new(&n, t.as_deref().and_then(PortType::parse).unwrap_or(PortType::Text))); }
            }
            "switch" => { for (i, (n, _)) in names(node.cfg("cases"), &["port", "name", "id"]).into_iter().enumerate() { ports.outputs.insert(i, PortSpec::new(&n, PortType::Flow)); } }
            "parallel" => {
                if node.cfg_str("mode") == Some("fixed") { let lines = names(node.cfg("lines"), &["name", "id"]); if !lines.is_empty() { ports.outputs.clear(); for (n, _) in lines { ports.outputs.push(PortSpec::new(&n, PortType::Json)); } } }
            }
            "jev" => {
                let extra: Vec<PortSpec> = match node.cfg_str("mode").unwrap_or("pick") {
                    "yes-no" => vec![PortSpec::new("yes", PortType::Flow), PortSpec::new("no", PortType::Flow)],
                    "score" => vec![PortSpec::new("score", PortType::Number)],
                    "rank" => vec![PortSpec::new("ranking", PortType::List(Box::new(PortType::Text)))],
                    _ => names(node.cfg("choices"), &["id", "name"]).into_iter().map(|(n, _)| PortSpec::new(&n, PortType::Flow)).collect(),
                };
                for (i, s) in extra.into_iter().enumerate() { ports.outputs.insert(i, s); }
            }
            "sub-workflow" => { if let Some(Value::Object(o)) = node.cfg("inputs") { for k in o.keys() { ports.inputs.push(PortSpec::new(k, PortType::Json)); } } }
            _ => {}
        }
        Some(ports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_signatures() {
        let r = DefaultRegistry::new();
        for k in ["start", "end", "input", "output", "note", "prompt", "jev", "loop", "approve", "sub-workflow", "switch", "join"] { assert!(r.knows(k), "{k}"); }
        assert!(!r.knows("acme/nothing"));
        let start = r.ports(&Node::new("n0", "start")).unwrap();
        assert_eq!(start.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["prompt", "inputs", "files", "sender", "project"]);
        assert_eq!(start.output("files").unwrap().ty.to_string(), "list<file>");
        assert!(r.ports(&Node::new("x", "end")).unwrap().input("result").unwrap().many);
        let jev = r.ports(&Node::new("j", "jev").with_config(json!({ "mode": "yes-no" }))).unwrap();
        assert!(jev.output("yes").unwrap().ty.is_flow() && jev.output("no").is_some() && jev.output("confidence").is_some());
        let sw = r.ports(&Node::new("s", "switch").with_config(json!({ "cases": [{ "port": "big" }, "small"] }))).unwrap();
        assert!(sw.output("big").is_some() && sw.output("small").is_some() && sw.output("default").is_some());
        let inp = r.ports(&Node::new("i", "input").with_config(json!({ "name": "x", "type": "list<text>" }))).unwrap();
        assert_eq!(inp.output("value").unwrap().ty.to_string(), "list<text>");
        let lp = r.ports(&Node::new("l", "loop")).unwrap();
        assert!(lp.input("back").unwrap().many && lp.input("in").unwrap().required);
    }

    #[test]
    fn manifest_register_and_versions() {
        let m: KindManifest = serde_json::from_value(json!({
            "schema": 1, "kind": "acme/pr-label", "version": "1.2.0", "title": "Label a pull request", "category": "Forgejo", "icon": "tag",
            "ports": { "in": [ { "name": "pr", "type": "json", "required": true } ], "out": [ { "name": "labels", "type": "list<text>" }, { "name": "error", "type": "error" } ] },
            "config": { "type": "object", "required": ["repo"], "properties": { "repo": { "type": "string" } }, "additionalProperties": false },
            "impl": { "type": "graph" }, "permissions": { "network": ["forgejo.local"] }, "idempotent": true, "future": { "x": 1 } })).unwrap();
        assert_eq!(m.major(), 1);
        assert_eq!(serde_json::to_value(&m).unwrap()["future"], json!({ "x": 1 }), "unknown manifest fields survive");
        let mut r = DefaultRegistry::new();
        r.register(m).unwrap();
        assert!(r.has_version("acme/pr-label", 1) && !r.has_version("acme/pr-label", 2));
        let n = Node::new("a", "acme/pr-label");
        assert_eq!(r.check_config(&n), vec!["`config.repo` is required".to_string()]);
        let n = n.with_config(json!({ "repo": 3, "zzz": 1 }));
        assert_eq!(r.check_config(&n).len(), 2);
        let mut bad = r.manifest("acme/pr-label").unwrap().clone();
        bad.kind = "prompt".into();
        assert!(r.register(bad.clone()).unwrap_err()[0].contains("built-in"));
        bad.kind = "x y".into(); bad.version = "one".into(); bad.ports.inputs[0].ty = "wat".into();
        assert_eq!(bad.problems().len(), 3);
        assert_eq!(parse_semver("2.1"), Some((2, 1, 0)));
        assert_eq!(parse_semver("a"), None);
    }

    #[test]
    fn schema_subset() {
        let s = json!({ "type": "object", "required": ["a"], "properties": { "a": { "type": "integer", "minimum": 1, "maximum": 3 }, "b": { "enum": ["x", "y"] }, "c": { "type": "array", "items": { "type": "string" } } } });
        let check = |v: Value| { let mut out = vec![]; schema_check(&s, &v, "config", &mut out); out };
        assert!(check(json!({ "a": 2, "b": "x", "c": ["q"] })).is_empty());
        assert_eq!(check(json!({ "a": 5 })).len(), 1);
        assert_eq!(check(json!({ "a": 1.5 })).len(), 1);
        assert_eq!(check(json!({ "b": "z", "c": [1] })).len(), 3);
        assert_eq!(check(json!([])).len(), 1);
    }
}
