//! `rea` (T-255): a reverse-engineering step in a workflow. REA itself is not here: the node only uses the host's
//! standalone RE tools (`re_start`, `re_status`, `re_put`, `re_exec`, `re_call`, `re_get`, `re_stop`), the same ones a
//! Work run, Jcode PGM's `/re` and the products offer. So any host that has those tools runs `rea` nodes, and the
//! scope's tool policy decides whether a workflow may use them (a workflow cannot grant itself REA).
//!
//! One node = one copy's life, unless it is given a copy: start a throw-away copy of the base image (waiting while it is
//! queued), put the `files` in, run `exec`, call the REA `tool`, take the `get` files out, stop the copy. The copy is
//! stopped on every way out (an error, a cancel) except `keep: true`, which hands its id on through the `copy` port
//! (the next `rea` node takes it on its `copy` input and then leaves it running too; stop it with a last node that has
//! `stop: true`).
use super::{boxed, render, text_of};
use crate::host::{BoxFut, NodeCall, NodeError, NodeKind, NodeOutput, ToolCtx, WorkflowHost};
use pgm_workflow_internal::Node;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// How long a queued copy is waited for when the node names no `queueSec`.
const QUEUE_WAIT_SEC: u64 = 600;

pub struct Rea;

impl NodeKind for Rea {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            let tool = call.cfg("tool").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
            let exec = call.cfg("exec").cloned().filter(|v| !v.is_null());
            let given = call.inputs.get("copy").map(text_of).filter(|s| !s.is_empty()).or_else(|| call.cfg("copy").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string));
            let stop_only = call.cfg("stop").and_then(Value::as_bool) == Some(true);
            if tool.is_none() && exec.is_none() && !stop_only && call.cfg("files").is_none() {
                return Err(NodeError::fatal("engine.config", format!("node `{}` does nothing: give it a REA `tool`, an `exec` command, `files` or `stop: true`", call.node.id)));
            }
            let t = Tools { call: &call, host };
            let (id, ours) = match given { Some(id) => (id, false), None => (t.start().await?, true) };
            // it stays running only when asked (`keep`), or when it was handed in (its owner node stops it); `stop` ends it
            let stays = !stop_only && (call.cfg("keep").and_then(Value::as_bool) == Some(true) || !ours);
            let work = t.work(&id, tool.as_deref(), exec.as_ref());
            let res = tokio::select! {
                r = work => r,
                _ = call.cancel.cancelled() => Err(NodeError::new("engine.cancelled", "cancelled")),
            };
            // a copy this node started never outlives a failure; otherwise it ends unless it stays
            if !stays || (res.is_err() && ours) { let _ = t.tool("re_stop", json!({ "id": id })).await; }
            let out = res?;
            Ok(if stays { out.with("copy", json!(id)) } else { out })
        })
    }
    /// A resumed run must not start a second copy behind a person's back: it asks first.
    fn idempotent(&self, _node: &Node) -> bool { false }
}

struct Tools<'a> { call: &'a NodeCall, host: &'a dyn WorkflowHost }

impl Tools<'_> {
    async fn tool(&self, name: &str, args: Value) -> Result<Value, NodeError> {
        let c = self.call;
        let ctx = ToolCtx { run: c.run.clone(), key: c.key.clone(), node: c.node.id.clone(), project: c.vars.get("project").and_then(Value::as_str).map(str::to_string) };
        self.host.call_tool(name.to_string(), args, ctx, c.cancel.clone()).await
    }

    /// A new copy, running (a queued one is waited for, polling its status).
    async fn start(&self) -> Result<String, NodeError> {
        let c = self.call;
        let purpose = match c.cfg("purpose").and_then(Value::as_str) { Some(p) => render(c, self.host, p, false)?, None => format!("workflow run {} node {}", c.run, c.node.id) };
        let r = self.tool("re_start", json!({ "purpose": purpose })).await?;
        let sb = r.get("sandbox").cloned().unwrap_or(r);
        let id = sb.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        if id.is_empty() { return Err(NodeError::new("rea.start", format!("node `{}`: the RE container did not start (no id in the answer)", c.node.id))); }
        let mut state = sb.get("state").and_then(Value::as_str).unwrap_or("running").to_string();
        let limit = c.cfg("queueSec").and_then(Value::as_u64).unwrap_or(QUEUE_WAIT_SEC);
        let mut waited = 0u64;
        while state != "running" {
            if matches!(state.as_str(), "gone" | "stopping") { return Err(NodeError::new("rea.start", format!("node `{}`: the RE container {id} ended before it ran", c.node.id))); }
            if waited >= limit {
                let _ = self.tool("re_stop", json!({ "id": id })).await;
                return Err(NodeError::new("rea.queue", format!("node `{}`: the RE container waited {limit} s in the queue (every copy was busy)", c.node.id)));
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                _ = c.cancel.cancelled() => { let _ = self.tool("re_stop", json!({ "id": id })).await; return Err(NodeError::new("engine.cancelled", "cancelled")); }
            }
            waited += 2;
            let s = self.tool("re_status", json!({ "id": id })).await?;
            state = s.get("sandbox").and_then(|x| x.get("state")).and_then(Value::as_str).unwrap_or("gone").to_string();
        }
        Ok(id)
    }

    /// Files in, the command, the REA tool, files out: `result` (the tool's answer), `output` (the command's), `files`.
    async fn work(&self, id: &str, tool: Option<&str>, exec: Option<&Value>) -> Result<NodeOutput, NodeError> {
        let c = self.call;
        let mut out = NodeOutput::new();
        for f in c.cfg("files").and_then(Value::as_array).cloned().unwrap_or_default() {
            let path = f.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let mut a = json!({ "id": id, "path": path });
            if let Some(b) = f.get("base64").and_then(Value::as_str) { a["base64"] = json!(b); } else { a["text"] = json!(render(c, self.host, &text_of(f.get("text").unwrap_or(&Value::Null)), false)?); }
            self.tool("re_put", a).await?;
        }
        if let Some(cmd) = exec {
            let cmd = match cmd { Value::String(s) => json!(render(c, self.host, s, false)?), v => v.clone() };
            let r = self.tool("re_exec", json!({ "id": id, "cmd": cmd, "timeoutSec": c.cfg("timeoutSec") })).await?;
            if c.cfg("failOnExit").and_then(Value::as_bool) != Some(false) && r.get("exit").and_then(Value::as_i64).is_some_and(|e| e != 0) {
                return Err(NodeError::new("rea.exec", format!("node `{}`: the command exited {}: {}", c.node.id, r["exit"], text_of(&r["stderr"]).chars().take(400).collect::<String>())));
            }
            out = out.with("output", r);
        }
        if let Some(tool) = tool {
            let args = match c.inputs.get("args").filter(|v| v.is_object()) { Some(a) => a.clone(), None => fill(c, self.host, c.cfg("args").unwrap_or(&json!({})))? };
            let r = self.tool("re_call", json!({ "id": id, "tool": tool, "args": args, "timeoutSec": c.cfg("timeoutSec") })).await?;
            if r.get("isError").and_then(Value::as_bool) == Some(true) {
                return Err(NodeError::new("rea.tool", format!("node `{}`: REA's `{tool}` answered an error: {}", c.node.id, content_text(&r).chars().take(400).collect::<String>())));
            }
            out = out.with("result", r.clone()).with("text", json!(content_text(&r)));
        }
        let gets: Vec<String> = c.cfg("get").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        if !gets.is_empty() {
            let mut files = Map::new();
            for p in gets { files.insert(p.clone(), self.tool("re_get", json!({ "id": id, "path": p })).await?); }
            out = out.with("files", Value::Object(files));
        }
        Ok(out)
    }
}

/// An MCP tool result's text parts, joined (`{content: [{type: "text", text}]}`); anything else as JSON.
fn content_text(r: &Value) -> String {
    match r.get("content").and_then(Value::as_array) {
        Some(parts) => parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        None => text_of(r),
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
