//! A fake host and a few fake node kinds, for tests (spec 8.2: "a test implements it with a fake host that has
//! scripted AI answers"). Nothing here touches a network. The hub's suite and the node kinds of T-201 use it too.
//!
//! [`FakeHost`] answers AI calls from a script per node id (or per instance key), records every call (when it started,
//! ended, whether it was cancelled) and how many ran at once, asks questions into a list, and has a clock that tests
//! can set. [`fake_engine`] is an [`Engine`] with `prompt` and `tool` given fake behaviour and two test kinds:
//! `test/inc` (adds a number) and `test/side-effect` (a tool call that is not idempotent).
use crate::cancel::Cancel;
use crate::engine::Engine;
use crate::host::{Ask, BoxFut, ModelAnswer, ModelRequest, NodeCall, NodeError, NodeKind, NodeOutput, ToolCtx, WorkflowHost};
use pgm_workflow_internal::KindManifest;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

/// One scripted answer.
#[derive(Debug, Clone)]
pub enum Reply {
    Text(String),
    /// Fails with this error code.
    Fail(String),
    Panic,
    /// Never answers (until cancelled).
    Hang,
    /// The answer after this many milliseconds.
    After(u64, Box<Reply>),
    /// The answer, costing this much (`usage.cost`).
    Cost(f64, Box<Reply>),
}

impl Reply {
    pub fn text(s: &str) -> Reply { Reply::Text(s.to_string()) }
    pub fn fail(code: &str) -> Reply { Reply::Fail(code.to_string()) }
    pub fn after(ms: u64, r: Reply) -> Reply { Reply::After(ms, Box::new(r)) }
    pub fn cost(c: f64, r: Reply) -> Reply { Reply::Cost(c, Box::new(r)) }
}

#[derive(Debug, Clone)]
pub struct FakeCall {
    pub node: String,
    pub key: String,
    pub attempt: u32,
    pub prompt: String,
    pub started: Instant,
    pub ended: Option<Instant>,
    pub cancelled: bool,
}

#[derive(Default)]
pub struct FakeHost {
    script: Mutex<HashMap<String, VecDeque<Reply>>>,
    delay: Mutex<HashMap<String, u64>>,
    pub calls: Mutex<Vec<FakeCall>>,
    running: AtomicUsize,
    most: AtomicUsize,
    pub asks: Mutex<Vec<Ask>>,
    pub emits: Mutex<Vec<String>>,
    pub tools: Mutex<Vec<(String, Value)>>,
    pub events: Mutex<Vec<Value>>,
    pub snapshots: Mutex<Vec<Value>>,
    refuse: AtomicBool,
    clock: AtomicU64,
}

impl FakeHost {
    pub fn new() -> Arc<FakeHost> { Arc::new(FakeHost::default()) }

    /// Queues an answer for a node id or an instance key (a key's queue is used first).
    pub fn reply(&self, node_or_key: &str, r: Reply) -> &Self { lock(&self.script).entry(node_or_key.to_string()).or_default().push_back(r); self }
    pub fn replies(&self, node_or_key: &str, rs: Vec<Reply>) -> &Self { for r in rs { self.reply(node_or_key, r); } self }
    /// Every call of this node takes this long.
    pub fn delay(&self, node: &str, ms: u64) -> &Self { lock(&self.delay).insert(node.to_string(), ms); self }
    /// Every AI and tool call fails (`engine.refused`): what a replay must never need.
    pub fn refuse_all(&self) -> &Self { self.refuse.store(true, Ordering::SeqCst); self }
    /// Sets the clock (0: the real clock).
    pub fn set_clock(&self, ms: u64) -> &Self { self.clock.store(ms, Ordering::SeqCst); self }
    pub fn advance(&self, ms: u64) -> &Self { self.clock.fetch_add(ms, Ordering::SeqCst); self }

    pub fn calls_of(&self, node: &str) -> Vec<FakeCall> { lock(&self.calls).iter().filter(|c| c.node == node).cloned().collect() }
    pub fn all_calls(&self) -> Vec<FakeCall> { lock(&self.calls).clone() }
    /// The most AI calls that were running at the same time.
    pub fn most_at_once(&self) -> usize { self.most.load(Ordering::SeqCst) }
    pub fn log(&self) -> Vec<Value> { lock(&self.events).clone() }

    fn next(&self, key: &str, node: &str) -> Option<Reply> {
        let mut s = lock(&self.script);
        if let Some(r) = s.get_mut(key).and_then(VecDeque::pop_front) { return Some(r); }
        s.get_mut(node).and_then(VecDeque::pop_front)
    }
}

struct Running<'a> { host: &'a FakeHost, i: usize, done: bool }
impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.host.running.fetch_sub(1, Ordering::SeqCst);
        let mut c = lock(&self.host.calls);
        c[self.i].ended = Some(Instant::now());
        if !self.done { c[self.i].cancelled = true; }
    }
}

impl WorkflowHost for FakeHost {
    fn call_model<'a>(&'a self, req: ModelRequest, cancel: Cancel) -> BoxFut<'a, Result<ModelAnswer, NodeError>> {
        Box::pin(async move {
            if self.refuse.load(Ordering::SeqCst) { return Err(NodeError::fatal("engine.refused", "this host refuses every call")); }
            let prompt = req.messages.iter().filter_map(|m| m.get("content").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
            let i = {
                let mut c = lock(&self.calls);
                c.push(FakeCall { node: req.node.clone(), key: req.key.clone(), attempt: req.attempt, prompt: prompt.clone(), started: Instant::now(), ended: None, cancelled: false });
                c.len() - 1
            };
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            let mut guard = Running { host: self, i, done: false };
            let mut reply = self.next(&req.key, &req.node).unwrap_or_else(|| Reply::Text(format!("ai:{}:{prompt}", req.node)));
            let mut wait = lock(&self.delay).get(&req.node).copied().unwrap_or(0);
            let mut cost = None;
            loop {
                match reply {
                    Reply::After(ms, r) => { wait += ms; reply = *r; }
                    Reply::Cost(c, r) => { cost = Some(c); reply = *r; }
                    _ => break,
                }
            }
            let hang = matches!(reply, Reply::Hang);
            if wait > 0 || hang {
                let sleep = tokio::time::sleep(if hang { Duration::from_secs(86_400) } else { Duration::from_millis(wait) });
                tokio::select! {
                    _ = sleep => {}
                    _ = cancel.cancelled() => return Err(NodeError::new("engine.cancelled", "cancelled")),
                }
            }
            guard.done = true;
            match reply {
                Reply::Text(t) => {
                    let usage = json!({ "input": prompt.len(), "output": t.len(), "cost": cost.unwrap_or(0.0) });
                    Ok(ModelAnswer { text: t, usage: Some(usage) })
                }
                Reply::Fail(code) => Err(NodeError::new(&code, format!("the fake AI failed ({code})"))),
                Reply::Panic => panic!("the fake AI panicked on purpose"),
                _ => Err(NodeError::new("engine.fake", "unreachable reply")),
            }
        })
    }

    fn call_tool<'a>(&'a self, name: String, args: Value, ctx: ToolCtx, _cancel: Cancel) -> BoxFut<'a, Result<Value, NodeError>> {
        Box::pin(async move {
            if self.refuse.load(Ordering::SeqCst) { return Err(NodeError::fatal("engine.refused", "this host refuses every call")); }
            lock(&self.tools).push((name.clone(), args.clone()));
            match self.next(&ctx.key, &ctx.node) {
                Some(Reply::Fail(code)) => Err(NodeError::new(&code, "the fake tool failed")),
                Some(Reply::Hang) => { tokio::time::sleep(Duration::from_secs(86_400)).await; Err(NodeError::new("engine.fake", "hung")) }
                Some(Reply::Text(t)) => Ok(json!(t)),
                _ => Ok(json!({ "tool": name, "args": args })),
            }
        })
    }

    fn ask_person(&self, ask: &Ask) -> Result<String, NodeError> {
        let mut a = lock(&self.asks);
        a.push(ask.clone());
        Ok(format!("q{}", a.len()))
    }

    fn emit(&self, _run: &str, _key: &str, message: &str) { lock(&self.emits).push(message.to_string()); }
    fn log(&self, event: &Value) { lock(&self.events).push(event.clone()); }
    fn save_snapshot(&self, _run: &str, snapshot: &Value) { lock(&self.snapshots).push(snapshot.clone()); }

    fn now_ms(&self) -> u64 {
        match self.clock.load(Ordering::SeqCst) {
            0 => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
            t => t,
        }
    }
}

fn text_of(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), x => x.to_string() } }

/// Stands in for T-201's `prompt`: the `user` template (else the `in` input as text) to [`WorkflowHost::call_model`];
/// outputs `text` and `usage`.
pub struct FakePrompt;
impl NodeKind for FakePrompt {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        Box::pin(async move {
            let text = match call.cfg("user").and_then(Value::as_str) { Some(t) => call.render(t)?, None => text_of(call.inputs.get("in").unwrap_or(&Value::Null)) };
            let req = ModelRequest { run: call.run.clone(), key: call.key.clone(), node: call.node.id.clone(), attempt: call.attempt, model: call.cfg("model").cloned(), messages: vec![json!({ "role": "user", "content": text })], options: json!({}) };
            let a = host.call_model(req, call.cancel.clone()).await?;
            let mut out = NodeOutput::port("text", json!(a.text));
            if let Some(u) = a.usage { out = out.with("usage", u.clone()).usage(u); }
            Ok(out)
        })
    }
}

/// Stands in for T-201's `tool`: `config.tool` with the `args` input.
pub struct FakeTool;
impl NodeKind for FakeTool {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        Box::pin(async move {
            let name = call.cfg("tool").and_then(Value::as_str).unwrap_or("echo").to_string();
            let ctx = ToolCtx { run: call.run.clone(), key: call.key.clone(), node: call.node.id.clone(), project: None };
            let r = host.call_tool(name, call.inputs.get("args").cloned().unwrap_or(Value::Null), ctx, call.cancel.clone()).await?;
            Ok(NodeOutput::port("result", r))
        })
    }
}

/// `test/inc`: the `in` number (or `in.n`) plus `config.by` (default 1), on `out`.
pub struct FakeInc;
impl NodeKind for FakeInc {
    fn run<'a>(&'a self, call: NodeCall, _host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        Box::pin(async move {
            let v = call.inputs.get("in").cloned().unwrap_or(Value::Null);
            let n = v.as_f64().or_else(|| v.get("n").and_then(Value::as_f64)).unwrap_or(0.0);
            let by = call.cfg("by").and_then(Value::as_f64).unwrap_or(1.0);
            let r = n + by;
            Ok(NodeOutput::port("out", if r.fract() == 0.0 { json!(r as i64) } else { json!(r) }))
        })
    }
}

/// `test/const`: `config.value` on `out`, whatever came in.
pub struct FakeConst;
impl NodeKind for FakeConst {
    fn run<'a>(&'a self, call: NodeCall, _host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        Box::pin(async move { Ok(NodeOutput::port("out", call.cfg("value").cloned().unwrap_or(Value::Null))) })
    }
}

fn manifest(v: Value) -> KindManifest { serde_json::from_value(v).expect("a test manifest") }

/// An engine with fake `prompt` and `tool` and the test kinds `test/inc`, `test/const` and `test/side-effect`.
pub fn fake_engine() -> Engine {
    let mut e = Engine::new();
    e.implement("prompt", Arc::new(FakePrompt));
    e.implement("tool", Arc::new(FakeTool));
    e.register(manifest(json!({ "schema": 1, "kind": "test/const", "version": "1.0.0", "ports": { "in": [{ "name": "in", "type": "json" }], "out": [{ "name": "out", "type": "json" }, { "name": "error", "type": "error" }] }, "config": { "type": "object" } })), Arc::new(FakeConst)).expect("test/const");
    e.register(manifest(json!({ "schema": 1, "kind": "test/inc", "version": "1.0.0", "ports": { "in": [{ "name": "in", "type": "json", "required": true }], "out": [{ "name": "out", "type": "json" }, { "name": "error", "type": "error" }] }, "config": { "type": "object" } })), Arc::new(FakeInc)).expect("test/inc");
    e.register(manifest(json!({ "schema": 1, "kind": "test/side-effect", "version": "1.0.0", "idempotent": false, "ports": { "in": [{ "name": "args", "type": "json", "required": true }], "out": [{ "name": "result", "type": "json" }, { "name": "error", "type": "error" }] }, "config": { "type": "object" } })), Arc::new(FakeTool)).expect("test/side-effect");
    e
}
