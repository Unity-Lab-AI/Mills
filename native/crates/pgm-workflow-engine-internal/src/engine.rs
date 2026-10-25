//! The engine: what products hold, and the four ways to drive a run (start, resume, answer, replay).
use crate::cancel::Cancel;
use crate::host::{NodeKind, WorkflowHost};
use crate::log::{Prior, RunLog};
use crate::plan::Plan;
use crate::run::{exec_scope, runnable, Limits, Mode, Run, Scope, ScopeEnd, Stop};
use pgm_workflow_internal::{validate, DefaultRegistry, Document, KindManifest, KindRegistry, Level, Node, Options};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;

/// What comes in (spec 1): the prompt, named inputs, files, who sent it, which project it targets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInput {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub inputs: Map<String, Value>,
    #[serde(default)]
    pub files: Vec<Value>,
    /// `{kind: "person"|"ai"|"trigger"|"workflow", id}`.
    #[serde(default)]
    pub sender: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl RunInput {
    pub fn prompt(p: &str) -> RunInput { RunInput { prompt: p.to_string(), ..Default::default() } }
    pub fn input(mut self, name: &str, v: Value) -> RunInput { self.inputs.insert(name.to_string(), v); self }
    pub fn sender(mut self, kind: &str, id: &str) -> RunInput { self.sender = json!({ "kind": kind, "id": id }); self }
    pub fn project(mut self, p: &str) -> RunInput { self.project = Some(p.to_string()); self }
}

/// A run's limits (spec 4.7). A document's `defaults.limits` sets them; what is set here is the scope policy's
/// ceiling (the lower one wins) and the default when the document says nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunLimits {
    pub max_seconds: Option<f64>,
    pub max_nodes: Option<u64>,
    pub max_cost: Option<f64>,
    pub max_parallel: Option<usize>,
}

#[derive(Clone, Default)]
pub struct RunOptions {
    pub run_id: Option<String>,
    /// `hub`, `local`, `gateway`, `hpm` (spec 8.1), written in `run_start`.
    pub runner: Option<String>,
    pub host_name: Option<String>,
    /// Cancelling it ends the run `cancelled` (spec 4.7).
    pub cancel: Cancel,
    pub limits: RunLimits,
    /// `{run, node}` when a `sub-workflow` node started this run.
    pub parent: Option<Value>,
}

/// How a drive of a run ended.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunReport {
    pub run: String,
    /// `ok`, `failed`, `cancelled`, `limit`, `gave-up` (the run ended: `run_end` is in the log), `waiting` (it waits for
    /// a person: [`RunReport::waiting`] says for what), or `invalid` (it never started: [`RunReport::problems`]).
    pub status: String,
    /// The name of the `end` that was reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(default)]
    pub waiting: Vec<Value>,
    #[serde(default)]
    pub totals: Value,
    #[serde(default)]
    pub problems: Vec<Value>,
    /// The whole log so far (earlier drives included).
    #[serde(default)]
    pub log: Vec<Value>,
}

impl RunReport {
    pub fn events(&self, ty: &str) -> Vec<&Value> { self.log.iter().filter(|e| e.get("type").and_then(Value::as_str) == Some(ty)).collect() }
}

/// Wraps a registered kind so its manifest's `idempotent: false` is honoured.
struct Manifested {
    inner: Arc<dyn NodeKind>,
    idempotent: bool,
}

impl NodeKind for Manifested {
    fn run<'a>(&'a self, call: crate::host::NodeCall, host: &'a dyn WorkflowHost) -> crate::host::BoxFut<'a, Result<crate::host::NodeOutput, crate::host::NodeError>> { self.inner.run(call, host) }
    fn idempotent(&self, node: &Node) -> bool { self.idempotent && self.inner.idempotent(node) }
}

/// Runs workflow documents. Holds the node kinds it knows (their shapes for validation, their behaviour for running).
#[derive(Clone)]
pub struct Engine {
    registry: Arc<DefaultRegistry>,
    kinds: Arc<HashMap<String, Arc<dyn NodeKind>>>,
}

impl Default for Engine {
    fn default() -> Self { Engine::new() }
}

impl Engine {
    /// The built-in kinds' shapes and the flow-control kinds' behaviour. The AI, tool, script and agent kinds have no
    /// behaviour until [`Engine::implement`] gives them one (T-201); a document using one is refused before it starts.
    pub fn new() -> Engine { Engine { registry: Arc::new(DefaultRegistry::new()), kinds: Arc::new(HashMap::new()) } }

    /// An engine with every built-in kind (T-201): `prompt`, `chat`, `summarise`, `jev`, `tool`, `script`, `agent`,
    /// `orchestrate`, besides the engine's own flow-control kinds and `sub-workflow`.
    pub fn with_builtins() -> Engine {
        use crate::kinds::{ai, orchestrate, script, tools};
        let mut e = Engine::new();
        e.implement("prompt", Arc::new(ai::Prompt));
        e.implement("chat", Arc::new(ai::Chat));
        e.implement("summarise", Arc::new(ai::Summarise));
        e.implement("jev", Arc::new(ai::Jev));
        e.implement("tool", Arc::new(tools::Tool));
        e.implement("agent", Arc::new(tools::Agent));
        e.implement("script", Arc::new(script::Script));
        e.implement("orchestrate", Arc::new(orchestrate::Orchestrate));
        e
    }

    /// Behaviour for a kind the registry already knows (a built-in one, such as `prompt`).
    pub fn implement(&mut self, kind: &str, k: Arc<dyn NodeKind>) { Arc::make_mut(&mut self.kinds).insert(kind.to_string(), k); }

    /// A new kind: its manifest (ports, config schema, `idempotent`) and its behaviour (a custom node, spec 11).
    pub fn register(&mut self, m: KindManifest, k: Arc<dyn NodeKind>) -> Result<(), Vec<String>> {
        let idempotent = m.idempotent.unwrap_or(true);
        let kind = m.kind.clone();
        Arc::make_mut(&mut self.registry).register(m)?;
        self.implement(&kind, Arc::new(Manifested { inner: k, idempotent }));
        Ok(())
    }

    pub fn registry(&self) -> &DefaultRegistry { &self.registry }

    fn prepare(&self, doc: &Document) -> (Vec<Value>, HashMap<Option<String>, Arc<Plan>>) {
        let mut problems: Vec<Value> = validate(doc, &Options::new(&*self.registry)).issues.into_iter().filter(|i| i.level == Level::Error)
            .map(|i| json!({ "code": i.code, "message": i.message, "node": i.node, "graph": i.graph })).collect();
        let graphs: Vec<(Option<String>, &[Node])> = std::iter::once((None, doc.nodes.as_slice()))
            .chain(doc.subgraphs.iter().flat_map(|m| m.iter().map(|(k, s)| (Some(k.clone()), s.nodes.as_slice())))).collect();
        for (g, nodes) in &graphs {
            for n in nodes.iter() {
                if !runnable(&self.kinds, &n.kind) && self.registry.manifest(&n.kind).is_some() {
                    problems.push(json!({ "code": "engine.kind-missing", "message": format!("node `{}`: this runner cannot run the kind `{}`", n.id, n.kind), "node": n.id, "graph": g }));
                }
            }
        }
        let mut plans = HashMap::new();
        if problems.is_empty() {
            for (g, _) in &graphs {
                match Plan::build(doc, g.as_deref(), &*self.registry) {
                    Ok(p) => { plans.insert(g.clone(), Arc::new(p)); }
                    Err(ps) => for (code, message) in ps { problems.push(json!({ "code": code, "message": message, "graph": g })); },
                }
            }
        }
        (problems, plans)
    }

    /// What would stop a document from running here (validation errors, kinds this engine cannot run, regions it
    /// cannot lay out). Empty: it may run. A run never starts half-able (spec 15, the 422).
    pub fn preflight(&self, doc: &Document) -> Vec<Value> { self.prepare(doc).0 }

    /// Starts a run and drives it until it ends or waits for a person.
    pub async fn start(&self, doc: Arc<Document>, input: RunInput, host: Arc<dyn WorkflowHost>, opts: RunOptions) -> RunReport {
        self.drive(doc, Some(input), host, opts, Prior::default(), Mode::Fresh, None).await
    }

    /// Goes on with a run from its log: after a restart, or to see whether a deadline has passed. Finished nodes are not
    /// run again; a node that was cut mid-run runs again, unless it is not idempotent (then a person is asked first).
    pub async fn resume(&self, doc: Arc<Document>, log: Vec<Value>, host: Arc<dyn WorkflowHost>, opts: RunOptions) -> RunReport {
        self.drive(doc, None, host, opts, Prior::from_log(log), Mode::Resume, None).await
    }

    /// Answers a question the run waits on (`key` as in its `wait` event) and goes on.
    #[allow(clippy::too_many_arguments)]
    pub async fn answer(&self, doc: Arc<Document>, log: Vec<Value>, key: &str, answers: Value, by: &str, host: Arc<dyn WorkflowHost>, opts: RunOptions) -> RunReport {
        let prior = Prior::from_log(log);
        if !prior.waits.contains_key(key) || prior.answers.contains_key(key) {
            return RunReport { run: prior.run.clone().unwrap_or_default(), status: "invalid".into(), problems: vec![json!({ "code": "engine.no-question", "message": format!("the run does not wait for an answer on `{key}`") })], log: prior.events, ..Default::default() };
        }
        self.drive(doc, None, host, opts, prior, Mode::Resume, Some((key.to_string(), answers, by.to_string()))).await
    }

    /// A new run made from an old one's log: every node gives its recorded result, nothing outside is called (spec 14).
    pub async fn replay(&self, doc: Arc<Document>, log: Vec<Value>, host: Arc<dyn WorkflowHost>, opts: RunOptions) -> RunReport {
        self.drive(doc, None, host, opts, Prior::from_log(log), Mode::Replay, None).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn drive(&self, doc: Arc<Document>, input: Option<RunInput>, host: Arc<dyn WorkflowHost>, opts: RunOptions, mut prior: Prior, mode: Mode, answer: Option<(String, Value, String)>) -> RunReport {
        let (problems, plans) = self.prepare(&doc);
        let invalid = |run: String, problems: Vec<Value>, log: Vec<Value>| RunReport { run, status: "invalid".into(), problems, log, ..Default::default() };
        if !problems.is_empty() { return invalid(prior.run.clone().or(opts.run_id.clone()).unwrap_or_default(), problems, prior.events); }
        if mode != Mode::Fresh && prior.start.is_none() { return invalid(prior.run.clone().unwrap_or_default(), vec![json!({ "code": "engine.no-run", "message": "the log has no run_start" })], prior.events); }
        if mode == Mode::Resume { if let Some(end) = prior.ended.clone() { return report_from_end(&prior, &end); } }
        let input = match (&input, &prior.start) {
            (Some(i), _) => i.clone(),
            (None, Some(s)) => RunInput {
                prompt: s.get("prompt").and_then(Value::as_str).unwrap_or("").to_string(),
                inputs: s.get("inputs").and_then(Value::as_object).cloned().unwrap_or_default(),
                files: s.get("files").and_then(Value::as_array).cloned().unwrap_or_default(),
                sender: s.get("sender").cloned().unwrap_or(Value::Null),
                project: s.get("project").and_then(Value::as_str).map(str::to_string),
            },
            _ => RunInput::default(),
        };
        let id = match mode {
            Mode::Resume => prior.run.clone().unwrap_or_default(),
            _ => opts.run_id.clone().unwrap_or_else(|| format!("hr-{:012x}", host.random() & 0xffff_ffff_ffff)),
        };
        let mut inputs = input.inputs.clone();
        for d in doc.inputs.iter().flatten() { if !inputs.contains_key(&d.name) { if let Some(v) = &d.default { inputs.insert(d.name.clone(), v.clone()); } } }
        let mut tmpl_inputs = inputs.clone();
        tmpl_inputs.entry("prompt".to_string()).or_insert_with(|| json!(input.prompt));
        let input_v = json!({ "prompt": input.prompt, "inputs": tmpl_inputs, "files": input.files, "sender": input.sender, "project": input.project });
        let limits = limits(&doc, &opts.limits);
        let runner = opts.runner.clone().unwrap_or_else(|| "local".into());
        let log = if mode == Mode::Resume { RunLog::new(&id, host.clone(), prior.last_seq, prior.events.clone()) } else { RunLog::new(&id, host.clone(), 0, vec![]) };
        let t0 = host.now_ms();
        if mode == Mode::Resume {
            log.emit("run_resume", json!({ "fromSeq": prior.last_seq, "runner": runner, "host": opts.host_name }));
            if let Some((key, answers, by)) = answer {
                let node = key.split('@').next().unwrap_or(&key).to_string();
                log.emit("answer", json!({ "node": node, "key": key, "by": by, "answers": answers }));
                let ev = log.events().last().cloned().unwrap_or(Value::Null);
                prior.answers.insert(key, ev);
            }
        } else {
            log.emit("run_start", json!({
                "workflow": { "id": doc.id, "name": doc.name, "version": doc.version, "scope": doc.scope },
                "runner": runner, "host": opts.host_name, "sender": input.sender, "project": input.project, "inputs": inputs, "prompt": input.prompt,
                "files": if input.files.is_empty() { Value::Null } else { json!(input.files) }, "parent": opts.parent,
                "replayOf": if mode == Mode::Replay { json!(prior.run) } else { Value::Null },
            }));
        }
        let resumed = mode == Mode::Resume;
        let run = Arc::new(Run {
            id: id.clone(), doc: doc.clone(), kinds: self.kinds.clone(), host: host.clone(), plans: plans.clone(), log,
            mode, cancel: Cancel::new(), stop: Mutex::new(None), sem: Arc::new(Semaphore::new(limits.max_parallel.max(1))),
            node_runs: (if resumed { prior.node_starts } else { 0 }).into(), cost: Mutex::new(if resumed { prior.cost } else { 0.0 }),
            limits: limits.clone(), prior: prior.clone(), state: Mutex::new(Map::new()), input: input_v, waiting: Mutex::new(vec![]),
            started: Mutex::new(Default::default()), engine: self.clone(),
            depth: opts.parent.as_ref().and_then(|p| p.get("depth")).and_then(Value::as_u64).unwrap_or(0) as usize,
        });
        let top = Scope::graph(plans[&None].clone(), String::new(), 0, run.cancel.child(), Map::new(), Map::new());
        let fut = exec_scope(run.clone(), top);
        tokio::pin!(fut);
        let earlier = if resumed { prior.active_ms } else { 0 };
        let rem = limits.max_seconds.map(|s| Duration::from_millis(((s * 1000.0) as u64).saturating_sub(earlier)));
        let timer = tokio::time::sleep(rem.unwrap_or(Duration::from_secs(u32::MAX as u64)));
        tokio::pin!(timer);
        let (mut timed, mut cancelled) = (rem.is_none(), false);
        let end = loop {
            tokio::select! {
                e = &mut fut => break e,
                _ = &mut timer, if !timed => { timed = true; run.limit("maxSeconds", json!(limits.max_seconds), None); }
                _ = opts.cancel.cancelled(), if !cancelled => { cancelled = true; run.halt(Stop::Cancelled); }
            }
        };
        let stop = run.stop.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let (status, outcome, result, error) = match (stop, end) {
            (Some(Stop::End { name, result }), _) => ("ok", Some(name), Some(result), None),
            (Some(Stop::Fail(e)), _) => ("failed", None, None, Some(e)),
            (Some(Stop::Limit { name, value }), _) => ("limit", None, None, Some(json!({ "code": "engine.limit", "limit": name, "value": value, "message": format!("the run reached its {name}") }))),
            (Some(Stop::GaveUp(e)), _) => ("gave-up", Some("gave-up".to_string()), None, Some(e)),
            (Some(Stop::Cancelled), _) => ("cancelled", None, None, None),
            (None, ScopeEnd::Pending) => ("waiting", None, None, None),
            (None, _) => ("failed", None, None, Some(json!({ "code": "engine.no-end", "message": "the run finished without reaching an end" }))),
        };
        let waiting = run.waiting.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let so_far = Prior::from_log(run.log.events());
        let totals = json!({ "nodes": so_far.nodes_done, "aiCalls": so_far.ai_calls, "tokens": so_far.tokens, "cost": so_far.cost });
        if status == "waiting" {
            run.log.emit("snapshot", json!({ "upTo": run.log.seq(), "file": "snapshot.json", "waiting": waiting }));
            host.save_snapshot(&id, &Prior::from_log(run.log.events()).snapshot());
        } else {
            let duration = earlier + host.now_ms().saturating_sub(t0);
            run.log.emit("run_end", json!({ "outcome": outcome, "status": status, "result": result, "error": error, "durationMs": duration, "totals": totals }));
        }
        RunReport { run: id, status: status.into(), outcome, result, error, waiting, totals, problems: vec![], log: run.log.events() }
    }
}

fn limits(doc: &Document, policy: &RunLimits) -> Limits {
    let d = doc.defaults.as_ref().and_then(|d| d.limits.clone()).unwrap_or(Value::Null);
    let f = |k: &str| d.get(k).and_then(Value::as_f64);
    let low = |a: Option<f64>, b: Option<f64>| match (a, b) { (Some(x), Some(y)) => Some(x.min(y)), (x, y) => x.or(y) };
    Limits {
        max_seconds: low(f("maxSeconds"), policy.max_seconds),
        max_nodes: low(f("maxNodes"), policy.max_nodes.map(|x| x as f64)).map_or(500, |x| x as u64),
        max_cost: low(f("maxCost"), policy.max_cost),
        max_parallel: low(f("maxParallel"), policy.max_parallel.map(|x| x as f64)).map_or(4, |x| x.max(1.0) as usize),
    }
}

fn report_from_end(prior: &Prior, end: &Value) -> RunReport {
    let s = |k: &str| end.get(k).and_then(Value::as_str).map(str::to_string);
    RunReport {
        run: prior.run.clone().unwrap_or_default(), status: s("status").unwrap_or_else(|| "failed".into()), outcome: s("outcome"),
        result: end.get("result").cloned(), error: end.get("error").cloned(), waiting: vec![], totals: end.get("totals").cloned().unwrap_or(Value::Null),
        problems: vec![], log: prior.events.clone(),
    }
}
