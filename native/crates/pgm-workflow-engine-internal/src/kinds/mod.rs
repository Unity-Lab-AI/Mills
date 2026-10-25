//! The built-in node kinds that call the outside (T-201; spec 5): `prompt`, `chat`, `summarise`, `jev` (ai.rs), `tool`
//! with custom HTTP calls, `agent` (tools.rs), `script` (script.rs), `orchestrate` (orchestrate.rs). They reach the
//! world only through the [`WorkflowHost`], so a test drives them with the fake host and the hub with its AI hosts and
//! providers. [`crate::Engine::with_builtins`] is an engine with all of them.
use crate::host::{BoxFut, ModelAnswer, ModelRequest, NodeCall, NodeError, WorkflowHost};
use pgm_workflow_internal::expr;
use serde_json::{json, Map, Value};

pub mod ai;
pub mod orchestrate;
pub mod script;
pub mod tools;

/// The model entries to try for a node, best first (spec 5.4): its own `model`, else its sections' (innermost, the
/// smallest, first), else the workflow's `defaults.model`; then that entry's `fallback` list. Empty: the host chooses
/// (the person's own default in Work).
pub fn model_chain(call: &NodeCall) -> Vec<Value> {
    model_chain_from(call, call.cfg("model").cloned())
}

/// The same, starting from a given entry (a `jev`'s second opinion, a chat's partner) when it is set.
pub fn model_chain_from(call: &NodeCall, own: Option<Value>) -> Vec<Value> {
    let mut first = own.filter(|m| m.is_object());
    if first.is_none() && call.graph.is_none() {
        let mut secs: Vec<_> = call.doc.sections.iter().flatten().filter(|s| s.nodes.contains(&call.node.id) && s.model.is_some()).collect();
        secs.sort_by_key(|s| s.nodes.len());
        first = secs.first().and_then(|s| s.model.clone());
    }
    if first.is_none() { first = call.doc.defaults.as_ref().and_then(|d| d.model.clone()); }
    let Some(m) = first else { return vec![] };
    let mut head = m.clone();
    if let Some(o) = head.as_object_mut() { o.remove("fallback"); }
    let mut out = vec![head];
    out.extend(m.get("fallback").and_then(Value::as_array).cloned().unwrap_or_default());
    out
}

/// A host error that means "try the next model" rather than "this failed" (spec 5.4: unreachable or rate-limited).
pub fn unreachable(e: &NodeError) -> bool { matches!(e.code.as_str(), "model.unreachable" | "model.rate-limited" | "engine.model-unreachable") }

/// One AI call down the node's model chain: the first entry that answers. Each entry that is unreachable moves on to
/// the next; when none is left the error is `engine.model-unreachable`.
pub async fn ask_model(call: &NodeCall, host: &dyn WorkflowHost, suffix: &str, chain: &[Value], messages: Vec<Value>, options: Value) -> Result<ModelAnswer, NodeError> {
    let list: Vec<Option<Value>> = if chain.is_empty() { vec![None] } else { chain.iter().cloned().map(Some).collect() };
    let mut last = None;
    for m in list {
        let req = ModelRequest {
            run: call.run.clone(), key: format!("{}{suffix}", call.key), node: format!("{}{suffix}", call.node.id), attempt: call.attempt,
            model: m, messages: messages.clone(), options: options.clone(),
        };
        match host.call_model(req, call.cancel.clone()).await {
            Ok(a) => return Ok(a),
            Err(e) if unreachable(&e) => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    let why = last.map(|e| e.message).unwrap_or_default();
    Err(NodeError::new("engine.model-unreachable", format!("node `{}`: no model of its chain answered ({why})", call.node.id)))
}

/// A `{{template}}` with the node's values, `{{prompt:name}}` from the host's saved prompts and `{{secret:NAME}}` from
/// its secrets (only where a kind says secrets may go: the headers of an HTTP call).
pub fn render(call: &NodeCall, host: &dyn WorkflowHost, template: &str, secrets: bool) -> Result<String, NodeError> {
    let bad = |e: expr::ExprError| NodeError::fatal("engine.template", format!("node `{}`: {e}", call.node.id));
    let t = expr::parse_template(template).map_err(bad)?;
    let look = |p: &[String]| -> Option<Value> {
        if let Some(name) = p.first().and_then(|f| f.strip_prefix("secret:")) { return if secrets { host.secret(name).map(Value::String) } else { None }; }
        call.lookup(p)
    };
    let prompts = |n: &str| host.saved_prompt(n);
    t.render(&look, Some(&prompts)).map_err(bad)
}

pub fn text_of(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), x => x.to_string() } }

/// Adds two usage objects (`input`, `output`, `cached`, `cost`).
pub fn add_usage(a: Option<Value>, b: &Option<Value>) -> Option<Value> {
    let Some(b) = b else { return a };
    let Some(a) = a else { return Some(b.clone()) };
    let mut out = Map::new();
    for k in ["input", "output", "cached"] { out.insert(k.into(), json!(a.get(k).and_then(Value::as_u64).unwrap_or(0) + b.get(k).and_then(Value::as_u64).unwrap_or(0))); }
    out.insert("cost".into(), json!(a.get("cost").and_then(Value::as_f64).unwrap_or(0.0) + b.get("cost").and_then(Value::as_f64).unwrap_or(0.0)));
    Some(Value::Object(out))
}

pub(crate) fn boxed<'a, T: Send + 'a>(f: impl std::future::Future<Output = T> + Send + 'a) -> BoxFut<'a, T> { Box::pin(f) }
