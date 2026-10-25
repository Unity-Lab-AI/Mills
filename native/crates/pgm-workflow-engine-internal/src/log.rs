//! The run log (spec 14) and what a run knows from an earlier log of itself.
//!
//! The log is append-only, one JSON object per event, `seq` gapless per run. It is also the run's memory: a resumed run
//! ([`Prior::from_log`]) takes every node instance that finished from its last `node_end` instead of running it again,
//! knows which questions are open and what was answered, and goes on numbering where the log stopped.
use crate::host::WorkflowHost;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(crate) struct RunLog {
    run: String,
    host: Arc<dyn WorkflowHost>,
    inner: Mutex<(u64, Vec<Value>)>,
}

impl RunLog {
    pub fn new(run: &str, host: Arc<dyn WorkflowHost>, last_seq: u64, events: Vec<Value>) -> RunLog {
        RunLog { run: run.to_string(), host, inner: Mutex::new((last_seq, events)) }
    }

    /// Writes one event (`v`, `seq`, `at`, `run`, `type` first, then `fields`) and hands it to the host. Returns its seq.
    pub fn emit(&self, ty: &str, fields: Value) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.0 += 1;
        let mut ev = Map::new();
        ev.insert("v".into(), json!(1));
        ev.insert("seq".into(), json!(g.0));
        ev.insert("at".into(), json!(self.host.now_ms()));
        ev.insert("run".into(), json!(self.run));
        ev.insert("type".into(), json!(ty));
        if let Value::Object(f) = fields { for (k, v) in f { if !v.is_null() { ev.insert(k, v); } } }
        let ev = Value::Object(ev);
        self.host.log(&ev);
        g.1.push(ev);
        g.0
    }

    pub fn seq(&self) -> u64 { self.inner.lock().unwrap_or_else(|e| e.into_inner()).0 }
    pub fn events(&self) -> Vec<Value> { self.inner.lock().unwrap_or_else(|e| e.into_inner()).1.clone() }
}

/// What an earlier log of a run says (for resume, answer and replay).
#[derive(Debug, Clone, Default)]
pub struct Prior {
    pub run: Option<String>,
    /// The `run_start` event.
    pub start: Option<Value>,
    /// The `run_end` event, when the run has ended.
    pub ended: Option<Value>,
    pub last_seq: u64,
    /// The last `node_end` of each node instance that is not followed by a retry: the instance is finished.
    pub memo: HashMap<String, Value>,
    /// Attempts started per instance (`node_start`s), and when the first one started.
    pub started: HashMap<String, (u32, u64)>,
    /// Open or past questions per instance (`wait`), and the answers given (`answer`).
    pub waits: HashMap<String, Value>,
    pub answers: HashMap<String, Value>,
    pub node_starts: u64,
    pub cost: f64,
    pub tokens: u64,
    pub ai_calls: u64,
    pub nodes_done: u64,
    /// Milliseconds the run was actually running (not waiting for a person) over its earlier drives.
    pub active_ms: u64,
    pub events: Vec<Value>,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> { v.get(k).and_then(Value::as_str) }

impl Prior {
    pub fn from_log(events: Vec<Value>) -> Prior {
        let mut p = Prior::default();
        let mut seg_start: Option<u64> = None;
        let mut last_at = 0u64;
        for ev in &events {
            let at = ev.get("at").and_then(Value::as_u64).unwrap_or(last_at);
            p.last_seq = p.last_seq.max(ev.get("seq").and_then(Value::as_u64).unwrap_or(0));
            if p.run.is_none() { p.run = s(ev, "run").map(str::to_string); }
            let key = s(ev, "key").or_else(|| s(ev, "node")).map(str::to_string);
            match s(ev, "type").unwrap_or("") {
                "run_start" | "run_resume" => {
                    if let Some(st) = seg_start { p.active_ms += last_at.saturating_sub(st); }
                    seg_start = Some(at);
                    if p.start.is_none() && s(ev, "type") == Some("run_start") { p.start = Some(ev.clone()); }
                }
                "snapshot" => { if let Some(st) = seg_start.take() { p.active_ms += at.saturating_sub(st); } }
                "node_start" => {
                    p.node_starts += 1;
                    if let Some(k) = key {
                        let e = p.started.entry(k.clone()).or_insert((0, at));
                        e.0 += 1;
                        p.memo.remove(&k);
                    }
                }
                "node_end" => {
                    if let Some(u) = ev.get("usage") {
                        p.ai_calls += 1;
                        p.tokens += u.get("input").and_then(Value::as_u64).unwrap_or(0) + u.get("output").and_then(Value::as_u64).unwrap_or(0);
                        p.cost += u.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
                    }
                    if ev.get("retry").and_then(Value::as_bool) != Some(true) {
                        if let Some(k) = key { p.memo.insert(k, ev.clone()); }
                        p.nodes_done += 1;
                    }
                }
                "wait" => { if let Some(k) = key { p.waits.insert(k, ev.clone()); } }
                "answer" => { if let Some(k) = key { p.answers.insert(k, ev.clone()); } }
                "run_end" => p.ended = Some(ev.clone()),
                _ => {}
            }
            last_at = at;
        }
        if let Some(st) = seg_start { p.active_ms += last_at.saturating_sub(st); }
        p.events = events;
        p
    }

    /// Instances that started and never finished nor wait for a person: they were running when the run was cut.
    pub fn in_flight(&self, key: &str) -> bool {
        self.started.contains_key(key) && !self.memo.contains_key(key) && !self.waits.contains_key(key)
    }

    /// The run's state rebuilt from the log (written to the host when a run stops to wait).
    pub fn snapshot(&self) -> Value {
        json!({
            "upTo": self.last_seq, "run": self.run, "activeMs": self.active_ms,
            "done": self.memo, "waits": self.waits, "answers": self.answers,
            "totals": { "nodeRuns": self.node_starts, "nodes": self.nodes_done, "aiCalls": self.ai_calls, "tokens": self.tokens, "cost": self.cost },
        })
    }
}
