//! The two traits the engine is driven through (spec 8.2).
//!
//! - [`WorkflowHost`]: everything outside the engine (AI calls, tools, agents, people, projects, secrets, the log sink,
//!   the clock). The hub implements it with pgm-ai-internal and pgm-collab-internal, jcode and HPM with their own
//!   tools, a test with [`crate::fake::FakeHost`]. Every method has a default ("not here"), so a host only implements
//!   what it has.
//! - [`NodeKind`]: what one kind of node does (prompt, tool, script, a custom node ...). The flow-control kinds (start,
//!   end, if, switch, parallel, join, loop, for-each, sub-graph, approve, wait, set, emit) are the engine's own; every
//!   other kind is a `NodeKind` given to [`crate::Engine::implement`] or [`crate::Engine::register`].
use crate::cancel::Cancel;
use pgm_workflow_internal::{expr, Document, Node};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A node's failure (spec 4.5: a failure is a value, not a crash). The engine adds the node and the attempt when it
/// puts it on the `error` port: `{code, message, node, attempt}`. Codes the engine makes start with `engine.`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeError {
    pub code: String,
    pub message: String,
    /// `Some(false)`: never retried, whatever the node's retry settings say (a denied tool, a bad config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    /// `{questions, state}`: the node cannot go on without a person ([`NodeError::ask`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Box<Value>>,
}

impl NodeError {
    pub fn new(code: &str, message: impl Into<String>) -> NodeError { NodeError { code: code.to_string(), message: message.into(), retryable: None, ask: None } }
    /// A failure no retry can mend.
    pub fn fatal(code: &str, message: impl Into<String>) -> NodeError { NodeError { code: code.to_string(), message: message.into(), retryable: Some(false), ask: None } }
    /// Not a failure: the node needs a person to answer `questions` (the question protocol's shape). The run waits with
    /// no worker held; when the answer comes the node runs again with [`NodeCall::answer`] set and [`NodeCall::asked`]
    /// holding `state` (what it had worked out, so it need not ask its AI again).
    pub fn ask(questions: Value, state: Value) -> NodeError {
        NodeError { code: "engine.ask".into(), message: "waiting for a person".into(), retryable: Some(false), ask: Some(Box::new(json!({ "questions": questions, "state": state }))) }
    }
    pub(crate) fn unavailable(what: &str) -> NodeError { NodeError::fatal("engine.unavailable", format!("this runner has no {what}")) }
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}: {}", self.code, self.message) }
}

/// What a node made: a value per output port that fired (a port left out does not fire), and what it used.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeOutput {
    pub outputs: Map<String, Value>,
    /// `{input, output, cached?, cost?}` (spec 14), for the run's totals and its `maxCost`.
    pub usage: Option<Value>,
}

impl NodeOutput {
    pub fn new() -> NodeOutput { NodeOutput::default() }
    pub fn port(name: &str, v: Value) -> NodeOutput { NodeOutput::new().with(name, v) }
    pub fn with(mut self, name: &str, v: Value) -> NodeOutput { self.outputs.insert(name.to_string(), v); self }
    pub fn usage(mut self, u: Value) -> NodeOutput { self.usage = Some(u); self }
}

/// One attempt of one node.
#[derive(Clone)]
pub struct NodeCall {
    pub run: String,
    /// The node instance: its id, plus where it runs when that is inside a loop round, a parallel line or a sub-graph
    /// (`n3@n2:2/n5:l1`). Stable across a resume, so a host can use it as an idempotency key.
    pub key: String,
    pub node: Node,
    pub attempt: u32,
    /// The input ports' values (a `many` port: a list).
    pub inputs: Map<String, Value>,
    /// What templates read: `inputs`, `node.<id>.out.<port>`, `state`, `loop`, `item`, `prompt`, `sender`, `project`.
    pub vars: Value,
    pub doc: Arc<Document>,
    /// The sub-graph the node is in (None: the top level).
    pub graph: Option<String>,
    pub cancel: Cancel,
    /// The person's answer, when this node asked one ([`NodeError::ask`]) and it came.
    pub answer: Option<Value>,
    /// The `state` the node gave with its question.
    pub asked: Option<Value>,
}

impl NodeCall {
    /// A value by path (`["node","n2","out","summary"]`), the input ports first (`["in"]`), then [`NodeCall::vars`].
    pub fn lookup(&self, path: &[String]) -> Option<Value> {
        let (first, rest) = path.split_first()?;
        let root = self.inputs.get(first).or_else(|| self.vars.get(first))?;
        walk(root, rest)
    }

    /// A `{{template}}` filled from [`NodeCall::lookup`]. A missing value without a default is an error (spec 5.1).
    pub fn render(&self, template: &str) -> Result<String, NodeError> {
        let look = |p: &[String]| self.lookup(p);
        expr::render(template, &look).map_err(|e| NodeError::fatal("engine.template", format!("node `{}`: {e}", self.node.id)))
    }

    /// The config value by key.
    pub fn cfg(&self, key: &str) -> Option<&Value> { self.node.cfg(key) }
}

/// Follows a path into a JSON value: object keys, list indexes.
pub fn walk(root: &Value, path: &[String]) -> Option<Value> {
    let mut cur = root;
    for p in path {
        cur = match cur {
            Value::Object(o) => o.get(p)?,
            Value::Array(a) => a.get(p.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur.clone())
}

/// What one kind of node does. One attempt per call: retries, timeouts, cancel, panics and the log are the engine's.
pub trait NodeKind: Send + Sync {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>>;
    /// May the node run again after a run was cut while it was running (spec 4.7)? When not, a resumed run asks a
    /// person first. Defaults to the manifest's `idempotent` for registered kinds, else yes.
    fn idempotent(&self, _node: &Node) -> bool { true }
}

/// An AI call (spec 5.1, 5.4): the model entry as the document has it (`{via, provider, model, fallback}`), resolved by
/// the node kind, then by the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub run: String,
    pub key: String,
    pub node: String,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub options: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelAnswer {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

/// A question for a person (spec 4.6; the question protocol of plans/question-protocol.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ask {
    pub run: String,
    pub key: String,
    pub node: String,
    /// `sender`, a username, a role, `anyone`.
    pub who: String,
    pub questions: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<u64>,
    /// `approval`, or `resume` (a node that is not idempotent was cut mid-run: run it again?).
    pub what: String,
}

/// A custom HTTP call (spec 5.2), already filled in (secrets included: the host must never log it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HttpAnswer {
    pub status: u16,
    pub body: String,
}

/// Which node is asking a host for a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCtx {
    pub run: String,
    pub key: String,
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// Everything outside the engine (spec 8.2).
pub trait WorkflowHost: Send + Sync {
    /// An AI call (through AI hosts, OpenRouter, Ollama ...). Should give up promptly when `cancel` fires.
    fn call_model<'a>(&'a self, _req: ModelRequest, _cancel: Cancel) -> BoxFut<'a, Result<ModelAnswer, NodeError>> {
        Box::pin(async { Err(NodeError::unavailable("AI")) })
    }
    /// A tool the scope allows (the host enforces the allowlist: a workflow cannot grant itself a tool).
    fn call_tool<'a>(&'a self, _name: String, _args: Value, _ctx: ToolCtx, _cancel: Cancel) -> BoxFut<'a, Result<Value, NodeError>> {
        Box::pin(async { Err(NodeError::unavailable("tools")) })
    }
    /// A custom HTTP call, only to the hosts the scope's policy allows (`network`, spec 9.3).
    fn http<'a>(&'a self, _req: HttpRequest, _cancel: Cancel) -> BoxFut<'a, Result<HttpAnswer, NodeError>> {
        Box::pin(async { Err(NodeError::unavailable("network")) })
    }
    /// A saved prompt's text by name or id (`{{prompt:name}}`, spec 6), as the workflow's scope sees it.
    fn saved_prompt(&self, _name: &str) -> Option<String> { None }
    /// The workflow a `sub-workflow` node runs: by name and version range (`3`, `stable`, none = published), from the
    /// store of the run's scope.
    fn workflow(&self, _name: &str, _range: Option<&str>) -> Option<Document> { None }
    /// A coding agent (a Work run, a jcode run): waits for its final answer.
    fn run_agent<'a>(&'a self, _spec: Value, _cancel: Cancel) -> BoxFut<'a, Result<Value, NodeError>> {
        Box::pin(async { Err(NodeError::unavailable("agents")) })
    }
    /// Raises a question and returns at once with its reference; the answer comes back through
    /// [`crate::Engine::answer`]. The run holds no worker while it waits.
    fn ask_person(&self, _ask: &Ask) -> Result<String, NodeError> { Err(NodeError::unavailable("way to ask a person")) }
    /// An `emit` node's message (to the sender, a chat, a session) while the run goes on.
    fn emit(&self, _run: &str, _key: &str, _message: &str) {}
    /// A project's path and worktree (spec 8.4).
    fn project(&self, _name: &str) -> Option<Value> { None }
    fn secret(&self, _name: &str) -> Option<String> { None }
    /// Every run log event, in `seq` order, as it is written (the run log sink, spec 14).
    fn log(&self, _event: &Value) {}
    /// The run's state when it stops to wait (or ends): a cache of what the log already says.
    fn save_snapshot(&self, _run: &str, _snapshot: &Value) {}
    fn now_ms(&self) -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }
    fn random(&self) -> u64 {
        use std::hash::{BuildHasher, Hasher};
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(self.now_ms() ^ N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).rotate_left(32));
        h.finish()
    }
}

/// A host with nothing: every outside call says "not here" (for runs of graphs that need none, and as a base).
pub struct NoHost;
impl WorkflowHost for NoHost {}

pub(crate) fn err_value(e: &NodeError, node: &str, attempt: u32) -> Value {
    json!({ "code": e.code, "message": e.message, "node": node, "attempt": attempt })
}
