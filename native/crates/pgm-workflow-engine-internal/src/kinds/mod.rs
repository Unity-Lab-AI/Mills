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

/// One AI call of a node, with its laws (spec 6.2): they go into the system part under a heading, and each law with a
/// `check` is run on the answer. A failed check follows the law's `onFail`: `retry` (asked again once, telling the AI
/// which law it broke, then `engine.law-check`), `error` (`engine.law-check`), `warn` (a warning in the node's log).
pub async fn ask_model(call: &NodeCall, host: &dyn WorkflowHost, suffix: &str, chain: &[Value], messages: Vec<Value>, options: Value) -> Result<ModelAnswer, NodeError> {
    let mut messages = with_laws(call, messages);
    let mut a = ask_chain(call, host, suffix, chain, messages.clone(), options.clone()).await?;
    let mut retried = false;
    'answer: loop {
        for law in call.laws.iter().filter(|l| l.check.is_some()) {
            let c = law.check.as_ref().unwrap();
            if law_holds(call, host, law, &a.text, chain).await? { continue; }
            match c.on_fail.as_str() {
                "warn" => call.warn(format!("the answer does not pass the check of law `{}` ({})", law.id, law.name)),
                "retry" if !retried => {
                    retried = true;
                    messages.push(json!({ "role": "assistant", "content": a.text }));
                    messages.push(json!({ "role": "user", "content": format!("Your answer breaks the law \"{}\": {}. Answer again, following it.", law.name, law.text) }));
                    let b = ask_chain(call, host, suffix, chain, messages.clone(), options.clone()).await?;
                    a = ModelAnswer { usage: add_usage(a.usage.clone(), &b.usage), text: b.text };
                    continue 'answer;
                }
                _ => return Err(NodeError::new("engine.law-check", format!("node `{}`: the answer breaks the law `{}` ({})", call.node.id, law.id, law.name))),
            }
        }
        return Ok(a);
    }
}

fn with_laws(call: &NodeCall, mut messages: Vec<Value>) -> Vec<Value> {
    if call.laws.is_empty() { return messages; }
    let text = format!("Laws (follow every one of them):\n{}", call.laws.iter().map(|l| format!("- {}: {}", l.name, l.text)).collect::<Vec<_>>().join("\n"));
    match messages.iter_mut().find(|m| m["role"] == "system") {
        Some(m) => { let old = m["content"].as_str().unwrap_or("").to_string(); m["content"] = json!(format!("{old}\n\n{text}")); }
        None => messages.insert(0, json!({ "role": "system", "content": text })),
    }
    messages
}

/// Does an answer pass a law's check? `regex`: the pattern is found in it; `expr`: the condition holds with `value`
/// the answer; `jev`: the decision model says yes to the check's `question` about it.
async fn law_holds(call: &NodeCall, host: &dyn WorkflowHost, law: &pgm_workflow_internal::shapes::Law, text: &str, chain: &[Value]) -> Result<bool, NodeError> {
    let c = law.check.as_ref().unwrap();
    let field = |k: &str| c.extra.get(k).and_then(Value::as_str).unwrap_or("");
    let bad = |e: expr::ExprError| NodeError::fatal("engine.law", format!("law `{}`: {e}", law.id));
    Ok(match c.kind.as_str() {
        "regex" => expr::regex_search(field("pattern"), text).map_err(bad)?,
        "expr" => {
            let look = |p: &[String]| -> Option<Value> { match p.first().map(String::as_str) { Some("value") | Some("answer") => Some(json!(text)), _ => None } };
            expr::parse_condition(field("expr")).map_err(bad)?.test(&look).map_err(bad)?
        }
        "jev" => {
            let ms = vec![
                json!({ "role": "system", "content": "You check an answer against a rule. Decide; do not discuss. Answer with JSON only: {\"choice\": \"yes\" or \"no\"}." }),
                json!({ "role": "user", "content": format!("Rule: {}\n\nAnswer:\n{text}\n\nDoes the answer follow the rule?", field("question")) }),
            ];
            let own = c.extra.get("model").cloned().filter(Value::is_object).map(|m| vec![m]);
            let a = ask_chain(call, host, &format!("#law-{}", law.id), own.as_deref().unwrap_or(chain), ms, json!({ "responseFormat": "json", "temperature": 0 })).await?;
            ai::json_in(&a.text).and_then(|v| v.get("choice").and_then(Value::as_str).map(|s| s.trim().eq_ignore_ascii_case("yes"))).unwrap_or(false)
        }
        _ => true,
    })
}

/// One AI call down a model chain: the first entry that answers. Each entry that is unreachable moves on to the next;
/// when none is left the error is `engine.model-unreachable`.
pub async fn ask_chain(call: &NodeCall, host: &dyn WorkflowHost, suffix: &str, chain: &[Value], messages: Vec<Value>, options: Value) -> Result<ModelAnswer, NodeError> {
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
