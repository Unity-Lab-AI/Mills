//! `orchestrate` (spec 5.5): a manager pattern in one box. A **planner** turns the goal into subtasks (JSON), the
//! subtasks run side by side (one AI call each, `#work<i>`), a **reviewer** says `done` or `again` with notes, and the
//! round repeats with those notes up to `maxRounds` (default 2). The planner and the reviewer use the node's model
//! chain. A worker uses `workerModel`, or, when `workers` holds a pgm-orchestrator profile (library law: the policy of
//! `/pgm-orchestrator`), the route that profile's `select` picks for the subtask's kind, its fallbacks after it.
use super::ai::json_in;
use super::{add_usage, ask_model, boxed, model_chain, model_chain_from, text_of};
use crate::host::{BoxFut, NodeCall, NodeError, NodeKind, NodeOutput, WorkflowHost};
use pgm_orchestrator_internal::health::Health;
use pgm_orchestrator_internal::profile::{Profile, TaskKind};
use pgm_orchestrator_internal::select::{infer_task_kind, select};
use serde_json::{json, Value};

/// `openrouter:z-ai/glm-5.2:free` -> `{via: "openrouter", model: "z-ai/glm-5.2:free"}`; a provider of the hub's own
/// (`claude-api:…`) goes `via: "host"` with that provider.
pub fn route_model(route: &str) -> Value {
    let (p, m) = route.split_once(':').unwrap_or(("", route));
    match p {
        "openrouter" | "ollama" => json!({ "via": p, "model": m, "route": route }),
        _ => json!({ "via": "host", "provider": p, "model": m, "route": route }),
    }
}

fn msg(role: &str, content: &str) -> Value { json!({ "role": role, "content": content }) }

pub struct Orchestrate;
impl NodeKind for Orchestrate {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            let id = call.node.id.clone();
            let goal = text_of(call.inputs.get("goal").unwrap_or(&Value::Null));
            let rounds_max = call.cfg("maxRounds").and_then(Value::as_u64).unwrap_or(2).clamp(1, 10);
            let tasks_max = call.cfg("maxTasks").and_then(Value::as_u64).unwrap_or(8).clamp(1, 32) as usize;
            let profile: Option<Profile> = match call.cfg("workers") {
                Some(w) => Some(serde_json::from_value(w.clone()).map_err(|e| NodeError::fatal("engine.config", format!("node `{id}`: `workers` is not a worker profile: {e}")))?),
                None => None,
            };
            let chain = model_chain(&call);
            let planner = call.cfg("planner").and_then(Value::as_str).unwrap_or("You plan work. Split the goal into a few independent subtasks that can be done side by side. Answer with JSON only: a list of strings, or of {\"task\": \"...\", \"kind\": \"research|summarize|implement|verify|debug|review|write|general\"}.").to_string();
            let worker = call.cfg("worker").and_then(Value::as_str).unwrap_or("You do one part of a larger goal. Do your part only, completely, and answer with the result.").to_string();
            let reviewer = call.cfg("reviewer").and_then(Value::as_str).unwrap_or("You review the parts against the goal. Answer with JSON only: {\"verdict\": \"done\" or \"again\", \"notes\": \"what is missing or wrong\"}.").to_string();
            let (mut usage, mut notes, mut verdict, mut round) = (None, String::new(), String::from("done"), 0u64);
            let (mut subtasks, mut results): (Vec<Value>, Vec<String>) = (vec![], vec![]);
            while round < rounds_max {
                round += 1;
                let ask = if notes.is_empty() { goal.clone() } else { format!("{goal}\n\nThe last round's review said: {notes}") };
                let p = ask_model(&call, host, "#plan", &chain, vec![msg("system", &planner), msg("user", &ask)], json!({ "responseFormat": "json" })).await?;
                usage = add_usage(usage, &p.usage);
                let list = json_in(&p.text).and_then(|v| v.as_array().cloned()).ok_or_else(|| NodeError::new("engine.plan-shape", format!("node `{id}`: the planner's answer is not a list of subtasks")))?;
                subtasks = list.into_iter().take(tasks_max).map(|t| match t { Value::String(s) => json!({ "task": s }), o => o }).filter(|t| t.get("task").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty())).collect();
                if subtasks.is_empty() { return Err(NodeError::new("engine.plan-empty", format!("node `{id}`: the planner gave no subtasks"))); }
                let calls = subtasks.iter().enumerate().map(|(i, t)| {
                    let task = t["task"].as_str().unwrap_or("").to_string();
                    let kind = t.get("kind").and_then(Value::as_str).and_then(TaskKind::parse).unwrap_or_else(|| infer_task_kind(&task));
                    let wchain: Vec<Value> = match &profile {
                        Some(pr) => select(pr, &Health::new(), kind, task.len() / 4 + 200, 0).routes.iter().map(|r| route_model(r)).collect(),
                        None => model_chain_from(&call, call.cfg("workerModel").cloned()),
                    };
                    let (call, worker, goal) = (&call, &worker, &goal);
                    async move { ask_model(call, host, &format!("#work{}", i + 1), &wchain, vec![msg("system", worker), msg("user", &format!("Goal: {goal}\n\nYour part: {task}"))], json!({})).await }
                });
                let answers = futures_util::future::join_all(calls).await;
                results = vec![];
                for a in answers { let a = a?; usage = add_usage(usage, &a.usage); results.push(a.text); }
                let parts = subtasks.iter().zip(&results).enumerate().map(|(i, (t, r))| format!("Part {} ({}):\n{r}", i + 1, t["task"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n\n");
                let r = ask_model(&call, host, "#review", &chain, vec![msg("system", &reviewer), msg("user", &format!("Goal: {goal}\n\n{parts}"))], json!({ "responseFormat": "json" })).await?;
                usage = add_usage(usage, &r.usage);
                let rv = json_in(&r.text).unwrap_or(Value::Null);
                verdict = rv.get("verdict").and_then(Value::as_str).map(|s| s.trim().to_lowercase()).unwrap_or_else(|| if r.text.to_lowercase().contains("again") { "again".into() } else { "done".into() });
                notes = rv.get("notes").and_then(Value::as_str).unwrap_or(&r.text).to_string();
                if verdict != "again" { break; }
            }
            let result = json!({ "subtasks": subtasks, "results": results, "verdict": verdict, "notes": notes, "rounds": round });
            let mut out = NodeOutput::port("result", result);
            if let Some(u) = usage { out = out.usage(u); }
            Ok(out)
        })
    }
}
