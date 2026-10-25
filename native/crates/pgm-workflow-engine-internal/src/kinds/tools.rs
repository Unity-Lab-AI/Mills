//! `tool` and `agent` (spec 5.2, 5.5): the kinds that act in the world, through the host only. The host enforces which
//! tools a scope allows (a workflow cannot grant itself a tool: a refused one is `engine.tool-denied`).
use super::{boxed, render, text_of};
use crate::host::{walk, BoxFut, HttpRequest, NodeCall, NodeError, NodeKind, NodeOutput, ToolCtx, WorkflowHost};
use pgm_workflow_internal::{expr, Node};
use serde_json::{json, Map, Value};

/// `tool`: a named tool (`config.tool`, arguments from the `args` input) or a custom HTTP call (`config.http`:
/// `{method, url, headers, body, expect: {status, path}}`). Templates fill the url, the headers and the body; only the
/// headers may name `{{secret:NAME}}`, and secrets are never in the log (the log has the node's inputs, not its
/// rendered call). `expect.status` (a number or a list; default any 2xx) is checked, `expect.path` picks the result out
/// of a JSON answer.
pub struct Tool;
impl NodeKind for Tool {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            if let Some(h) = call.cfg("http").cloned() { return http(&call, host, &h).await; }
            let name = call.cfg("tool").and_then(Value::as_str).ok_or_else(|| NodeError::fatal("engine.config", format!("node `{}` names no `tool` (or an `http` call)", call.node.id)))?.to_string();
            let ctx = ToolCtx { run: call.run.clone(), key: call.key.clone(), node: call.node.id.clone(), project: call.vars.get("project").and_then(Value::as_str).map(str::to_string) };
            let args = call.inputs.get("args").cloned().or_else(|| call.cfg("args").cloned()).unwrap_or(json!({}));
            let r = host.call_tool(name, args, ctx, call.cancel.clone()).await?;
            Ok(NodeOutput::port("result", r))
        })
    }
    fn idempotent(&self, node: &Node) -> bool {
        // a GET reads; anything else may change something, and so may a named tool unless it says it only reads
        match node.cfg("http") { Some(h) => h.get("method").and_then(Value::as_str).unwrap_or("GET").eq_ignore_ascii_case("GET"), None => node.cfg("idempotent").and_then(Value::as_bool).unwrap_or(false) }
    }
}

fn fill(call: &NodeCall, host: &dyn WorkflowHost, v: &Value) -> Result<Value, NodeError> {
    Ok(match v {
        Value::String(s) if s.contains("{{") => Value::String(render(call, host, s, false)?),
        Value::Array(a) => Value::Array(a.iter().map(|x| fill(call, host, x)).collect::<Result<_, _>>()?),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| Ok((k.clone(), fill(call, host, x)?))).collect::<Result<Map<_, _>, NodeError>>()?),
        x => x.clone(),
    })
}

async fn http(call: &NodeCall, host: &dyn WorkflowHost, h: &Value) -> Result<NodeOutput, NodeError> {
    let method = h.get("method").and_then(Value::as_str).unwrap_or("GET").to_uppercase();
    let url = render(call, host, h.get("url").and_then(Value::as_str).unwrap_or(""), false)?;
    if !(url.starts_with("https://") || url.starts_with("http://")) { return Err(NodeError::fatal("engine.config", format!("node `{}`: `{url}` is not an http(s) address", call.node.id))); }
    let mut headers = vec![];
    for (k, v) in h.get("headers").and_then(Value::as_object).cloned().unwrap_or_default() { headers.push((k, render(call, host, &text_of(&v), true)?)); }
    let body = match h.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(render(call, host, s, false)?),
        Some(v) => Some(fill(call, host, v)?.to_string()),
    };
    let a = host.http(HttpRequest { method, url, headers, body }, call.cancel.clone()).await?;
    let expect = h.get("expect").cloned().unwrap_or(json!({}));
    let ok = match expect.get("status") {
        Some(Value::Number(n)) => n.as_u64() == Some(a.status as u64),
        Some(Value::Array(l)) => l.iter().any(|x| x.as_u64() == Some(a.status as u64)),
        _ => (200..300).contains(&a.status),
    };
    let parsed: Value = serde_json::from_str(&a.body).unwrap_or_else(|_| Value::String(a.body.clone()));
    if !ok { return Err(NodeError::new("engine.http-status", format!("node `{}`: the call answered {}", call.node.id, a.status))); }
    let result = match expect.get("path").and_then(Value::as_str) {
        Some(p) => {
            let path = expr::parse_path(p).map_err(|e| NodeError::fatal("engine.config", format!("node `{}`: expect.path: {e}", call.node.id)))?;
            walk(&parsed, &path).ok_or_else(|| NodeError::new("engine.http-path", format!("node `{}`: the answer has no `{p}`", call.node.id)))?
        }
        None => parsed,
    };
    Ok(NodeOutput::port("result", result).with("status", json!(a.status)))
}

/// `agent`: hands one task to a coding agent (a Work run, a jcode run) and **waits for its final answer** (spec 5.5).
/// The agent loop is inside this node: it cannot change the graph. Config: `project` (else the run's), `tools` (its
/// allowlist), `timeoutSec`, `schema` (the shape of the answer it must give), `model`.
pub struct Agent;
impl NodeKind for Agent {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            let task = text_of(call.inputs.get("task").unwrap_or(&Value::Null));
            let mut spec = json!({ "run": call.run, "key": call.key, "node": call.node.id, "task": task });
            spec["project"] = call.cfg("project").cloned().or_else(|| call.vars.get("project").cloned()).unwrap_or(Value::Null);
            for k in ["tools", "timeoutSec", "schema", "model", "provider"] { if let Some(v) = call.cfg(k) { spec[k] = v.clone(); } }
            let r = host.run_agent(spec, call.cancel.clone()).await?;
            let text = r.get("text").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| text_of(&r));
            Ok(NodeOutput::port("result", r.get("result").cloned().unwrap_or_else(|| r.clone())).with("text", json!(text)))
        })
    }
    fn idempotent(&self, _node: &Node) -> bool { false }
}
