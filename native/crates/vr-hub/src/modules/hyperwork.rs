//! Hyper Workflows on the hub (T-204; the team note plans/hyper-workflows.md): pgm-workflow-engine-internal's store and
//! run API under `/api/hyperwork/`, with this hub's people ([`HubDir`]) and this hub's AI providers, tools and HTTP
//! ([`HubHost`]). Its data is `Hyperwork/` next to `Board/` and `Notes/` (gitignored by the whitelist: data, not code).
//! Everything is in the Rust hub only: the frozen JavaScript hub answers 404 on these paths.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

use super::{answer, stream_body, Routed};
use crate::front::{Hub, Req};
use pgm_ai_internal::hub::AiHub;
use pgm_identity_internal::Account;
use pgm_workflow_engine_internal::service::{Answer, Caller, Directory, Service};
use pgm_workflow_engine_internal::{BoxFut, Cancel, Engine, HttpAnswer, HttpRequest, ModelAnswer, ModelRequest, NodeError, ToolCtx, WorkflowHost};

/// The service, made once the hub exists (it needs the hub for people and AI).
#[derive(Default)]
pub struct Hyperwork { svc: OnceLock<Arc<Service>> }

impl Hyperwork {
    pub fn get(&self) -> Option<&Arc<Service>> { self.svc.get() }
}

/// Starts the service: runs that were running when the hub stopped go on from their logs; waiting runs whose deadline
/// passes are driven every few seconds (their timeouts).
pub fn start(hub: &Arc<Hub>) {
    let dir: Arc<dyn Directory> = Arc::new(HubDir { hub: Arc::downgrade(hub) });
    let svc = Service::new(hub.mods.mills_dir().join("Hyperwork"), Engine::with_builtins(), dir);
    if hub.mods.hyperwork.svc.set(svc.clone()).is_err() { return; }
    svc.resume_all(true);
    super::forever("hyperwork deadlines", move || { let svc = svc.clone(); async move { loop { tokio::time::sleep(std::time::Duration::from_secs(5)).await; svc.tick(); } } });
}

/// A client that may only list, view and run (spec 9.2): jcode, by the client its session signed in with, or any client
/// that says so itself (a client can always ask for less).
fn run_only(hub: &Hub, r: &Req) -> bool {
    let kind = hub.mods.auth.session_client(r).and_then(|c| c["kind"].as_str().or(c["name"].as_str()).map(str::to_lowercase)).unwrap_or_default();
    matches!(kind.as_str(), "jcode" | "jcodepgm" | "jcode-pgm" | "run-only") || matches!(r.header("x-vr-client").map(str::to_lowercase).as_deref(), Some("jcode" | "run-only"))
}

pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account, rest: &str) -> Routed {
    handle(hub, r, Caller { user: a.username.clone(), run_only: run_only(hub, r) }, rest).await
}

/// The same routes for a product through the gateway (`/internal/gateway/hyperwork/<rest>`, T-207 / spec 15): the person
/// is the one the product signed for (already checked as linked); a product that says `x-vr-client: run-only` gets a
/// run-only caller, else the person's full rights (Hyper Prod Man designs too; jcode is run-only by its session).
pub async fn via_gateway(hub: &Arc<Hub>, r: &Req, a: &Account, rest: &str) -> Routed {
    let run_only = matches!(r.header("x-vr-client").map(str::to_lowercase).as_deref(), Some("jcode" | "run-only"));
    handle(hub, r, Caller { user: a.username.clone(), run_only }, rest).await
}

async fn handle(hub: &Arc<Hub>, r: &Req, caller: Caller, rest: &str) -> Routed {
    let Some(svc) = hub.mods.hyperwork.get().cloned() else { return answer(503, json!({ "error": "Hyper Workflows are starting." })) };
    let query: HashMap<String, String> = crate::front::url_params(&r.query).into_iter().collect();
    let body = if r.method == hyper::Method::POST {
        if r.body.is_empty() { json!({}) } else { match serde_json::from_slice::<Value>(&r.body) { Ok(v) => v, Err(_) => return answer(400, json!({ "error": "That request is not valid JSON." })) } }
    } else { json!({}) };
    match svc.handle(&caller, r.method.as_str(), rest, &query, &body).await {
        Answer::Json(status, v) => answer(status, v),
        Answer::Lines(mut rx) => {
            let (tx, out) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, String>>(256);
            tokio::spawn(async move { while let Some(l) = rx.recv().await { if tx.send(Ok(bytes::Bytes::from(l))).await.is_err() { return; } } });
            Routed::Answer(hyper::Response::builder().status(200).header("content-type", "application/x-ndjson").header("cache-control", "no-store").body(stream_body(out)).expect("response"))
        }
    }
}

/// `POST /hooks/hyperwork/<id>` (T-212): a Forgejo / Gitea / GitHub webhook. No session: its signature (the trigger's
/// secret) is the key.
pub fn webhook(hub: &Arc<Hub>, r: &Req, id: &str) -> Routed {
    let Some(svc) = hub.mods.hyperwork.get().cloned() else { return answer(503, json!({ "error": "Hyper Workflows are starting." })) };
    if r.body.len() > 2_000_000 { return answer(413, json!({ "error": "That payload is too large." })); }
    let header = |k: &str| r.header(k).map(str::to_string);
    let (status, v) = svc.webhook(id, &header, &r.body);
    answer(status, v)
}

/// `POST /internal/hyperwork/event {name, payload}` (T-212's internal sockets): another process on this machine says
/// something happened; every `event` trigger of that name starts its workflow. From this machine only, with the
/// config's `hyperworkEventSecret` in `X-VR-Event-Secret`.
pub fn event(hub: &Arc<Hub>, r: &Req) -> Routed {
    use subtle::ConstantTimeEq;
    let Some(svc) = hub.mods.hyperwork.get().cloned() else { return answer(503, json!({ "error": "Hyper Workflows are starting." })) };
    let secret = hub.cfg.get("hyperworkEventSecret").and_then(Value::as_str).unwrap_or("");
    let given = r.header("x-vr-event-secret").unwrap_or("");
    if !r.peer.ip().is_loopback() || secret.len() < 16 || !bool::from(given.as_bytes().ct_eq(secret.as_bytes())) { return answer(403, json!({ "error": "forbidden" })); }
    let b = r.json();
    let name = b["name"].as_str().unwrap_or("");
    if name.is_empty() { return answer(400, json!({ "error": "Which event? (name)" })); }
    answer(200, json!({ "ok": true, "runs": svc.event(name, &b["payload"]) }))
}

/// The team tools `hyperwork_list` and `hyperwork_run` (bridge/team-mcp.js, mcp-tools.json): a Work run's AI sends a
/// prompt through a workflow as the person whose run it is (their rights, their run). `wait` (default 120 s, at most
/// 600) waits for the result; a run that waits for an approval answers with where it waits.
pub async fn tool(hub: &Arc<Hub>, account: &str, b: &Value) -> Value {
    let Some(svc) = hub.mods.hyperwork.get().cloned() else { return json!({ "ok": false, "error": "Hyper Workflows are starting." }) };
    let c = Caller { user: account.to_string(), run_only: false };
    let args = &b["args"];
    let fail = |v: &Value| json!({ "ok": false, "error": v["error"].as_str().unwrap_or("the hub refused it").to_string() + &v["nearest"].as_array().filter(|a| !a.is_empty()).map(|a| format!(" (nearest: {})", a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))).unwrap_or_default() });
    match b["name"].as_str().unwrap_or("") {
        "hyperwork_list" => {
            let q: HashMap<String, String> = args["q"].as_str().map(|q| [("q".to_string(), q.to_string())].into_iter().collect()).unwrap_or_default();
            match svc.handle(&c, "GET", "list", &q, &json!({})).await {
                Answer::Json(200, v) => json!({ "ok": true, "result": v["items"].as_array().map(|a| a.iter().map(|i| json!({ "name": i["shown"], "scope": i["scope"], "title": i["title"], "description": i["description"], "version": i["version"], "unrunnable": i["unrunnable"] })).collect::<Vec<_>>()).unwrap_or_default() }),
                Answer::Json(_, v) => fail(&v),
                Answer::Lines(_) => json!({ "ok": false, "error": "unexpected" }),
            }
        }
        "hyperwork_run" => {
            let mut body = json!({ "name": args["name"], "prompt": args["prompt"].as_str().unwrap_or("") });
            for k in ["inputs", "project"] { if !args[k].is_null() { body[k] = args[k].clone(); } }
            let run = match svc.handle(&c, "POST", "run", &HashMap::new(), &body).await {
                Answer::Json(202, v) => v["run"].as_str().unwrap_or("").to_string(),
                Answer::Json(_, v) => return fail(&v),
                Answer::Lines(_) => return json!({ "ok": false, "error": "unexpected" }),
            };
            let wait = args["wait"].as_f64().unwrap_or(120.0).clamp(0.0, 600.0);
            let until = std::time::Instant::now() + std::time::Duration::from_secs_f64(wait);
            let q: HashMap<String, String> = [("id".to_string(), run.clone())].into_iter().collect();
            loop {
                let m = match svc.handle(&c, "GET", "run", &q, &json!({})).await { Answer::Json(_, v) => v, Answer::Lines(_) => Value::Null };
                let status = m["status"].as_str().unwrap_or("running").to_string();
                if status != "running" || std::time::Instant::now() >= until {
                    return json!({ "ok": true, "result": { "run": run, "status": status, "outcome": m["outcome"], "result": m["result"], "error": m["error"], "waiting": m["waiting"] } });
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        n => json!({ "ok": false, "error": format!("Unknown team tool: {n}") }),
    }
}

/// Who is who on this hub: every account is in `company`; teams come from config `hyperworkTeams`
/// (`{ "core": ["gee", "sponge", "mills"] }` makes `team-core`); everyone has their own `me-<user>`.
pub struct HubDir { hub: Weak<Hub> }

impl Directory for HubDir {
    fn exists(&self, user: &str) -> bool { self.hub.upgrade().is_some_and(|h| h.mods.has(user)) }
    fn is_admin(&self, user: &str) -> bool { self.hub.upgrade().is_some_and(|h| h.mods.auth.is_admin(user)) }
    fn scopes_of(&self, user: &str) -> Vec<String> {
        let mut out = vec![format!("me-{user}")];
        if let Some(h) = self.hub.upgrade() {
            if let Some(teams) = h.cfg.get("hyperworkTeams").and_then(Value::as_object) {
                for (t, members) in teams {
                    if members.as_array().is_some_and(|m| m.iter().any(|x| x.as_str() == Some(user))) && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { out.push(format!("team-{t}")); }
                }
            }
        }
        out.push("company".into());
        out
    }
    fn host_for(&self, user: &str) -> Arc<dyn WorkflowHost> { Arc::new(HubHost { hub: self.hub.clone(), user: user.to_string() }) }
    fn changed(&self, user: &str, event: &Value) {
        if event["type"] == "hyper_question" { println!("[vr-hub] hyperwork: {user}'s run {} waits for an answer ({})", event["run"].as_str().unwrap_or(""), event["key"].as_str().unwrap_or("")); }
    }
    fn default_tools(&self) -> Vec<String> { READ_TOOLS.iter().map(|s| s.to_string()).collect() }
    /// The board's own repeats (month-end aware, this machine's local time): one scheduler's rules, not two.
    fn next_due(&self, ms: u64, recur: &str) -> Option<u64> {
        pgm_collab_internal::board::next_due(ms as i64, if recur == "nightly" { "daily" } else { recur }, 0).map(|x| x as u64)
    }
    /// The first `HH:MM` after `ms`, in this machine's local time (as the board's due dates).
    fn first_time(&self, ms: u64, at: &str) -> Option<u64> {
        use chrono::{Local, TimeZone};
        let (h, m) = at.split_once(':')?;
        let t = chrono::NaiveTime::from_hms_opt(h.trim().parse().ok()?, m.trim().parse().ok()?, 0)?;
        let now = Local.timestamp_millis_opt(ms as i64).single()?;
        for d in 0..2 {
            let day = now.date_naive() + chrono::Duration::days(d);
            if let Some(x) = Local.from_local_datetime(&day.and_time(t)).earliest() { if x.timestamp_millis() as u64 > ms { return Some(x.timestamp_millis() as u64); } }
        }
        None
    }
    /// A triggered run failed: a private chat message to its owner, from "Hyper Workflows".
    fn alert(&self, user: &str, text: &str) {
        if let Some(h) = self.hub.upgrade() { let _ = AiHub::agent_say(&*h, text, user, "Hyper Workflows", "", ""); }
    }
    fn secret_values(&self) -> Vec<String> {
        self.hub.upgrade().and_then(|h| h.cfg.get("hyperworkSecrets").and_then(Value::as_object).map(|m| m.values().filter_map(Value::as_str).map(str::to_string).collect())).unwrap_or_default()
    }
}

/// The tools a run may use when its scope's policy names none: reading the board and the notes.
const READ_TOOLS: &[&str] = &["board_list", "task_get", "board_calendar", "notes_list", "notes_read"];

/// A run's way out, as one person: their AI providers (spec 5.4), the board and notes tools, HTTP to the hosts the
/// config allows (`hyperworkNetwork`), the hub's workflow secrets (`hyperworkSecrets`).
pub struct HubHost { hub: Weak<Hub>, user: String }

impl HubHost {
    fn ai(&self) -> Option<Arc<dyn AiHub>> { self.hub.upgrade().and_then(|h| h.me()) }
}

fn unreachable(m: String) -> NodeError { NodeError::new("model.unreachable", m) }

impl WorkflowHost for HubHost {
    fn call_model<'a>(&'a self, req: ModelRequest, cancel: Cancel) -> BoxFut<'a, Result<ModelAnswer, NodeError>> {
        Box::pin(async move {
            let Some(ai) = self.ai() else { return Err(unreachable("the hub is stopping".into())) };
            let entry = req.model.clone().unwrap_or(Value::Null);
            let via = entry["via"].as_str().unwrap_or("host");
            let model = entry["model"].as_str().unwrap_or("").to_string();
            let provider = match via {
                "host" => entry["provider"].as_str().unwrap_or("").to_string(),
                // OpenRouter / Ollama: a provider of the person's (or shared) that is one
                v => {
                    let list = pgm_ai_internal::work::providers_list(&*ai, &self.user);
                    let found = list.iter().find(|p| [p["id"].as_str(), p["label"].as_str(), p["baseUrl"].as_str(), p["kind"].as_str()].iter().flatten().any(|x| x.to_lowercase().contains(v)));
                    match found { Some(p) => p["id"].as_str().unwrap_or("").to_string(), None => return Err(unreachable(format!("{} has no {v} provider (Settings → AI providers)", self.user))) }
                }
            };
            let system: Vec<String> = req.messages.iter().filter(|m| m["role"] == "system").filter_map(|m| m["content"].as_str().map(str::to_string)).collect();
            let msgs: Vec<(String, String)> = req.messages.iter().filter(|m| m["role"] != "system").map(|m| (m["role"].as_str().unwrap_or("user").to_string(), m["content"].as_str().unwrap_or("").to_string())).collect();
            let oc = Arc::new(pgm_ai_internal::openai::Cancel::default());
            let (oc2, c2) = (oc.clone(), cancel.clone());
            let watch = tokio::spawn(async move { c2.cancelled().await; oc2.fire(); });
            let max = req.options["maxTokens"].as_u64();
            let res = pgm_ai_internal::openai::answer_for(&ai, &self.user, &provider, &model, &system.join("\n\n"), &msgs, oc, max).await;
            watch.abort();
            match res {
                Ok((text, used, usage)) => Ok(ModelAnswer { text, usage: Some(json!({ "input": usage["prompt_tokens"], "output": usage["completion_tokens"], "model": used })) }),
                Err(e) if e.status == 429 => Err(NodeError::new("model.rate-limited", e.msg)),
                Err(e) if e.status == 503 => Err(unreachable(e.msg)),
                Err(e) => Err(NodeError::new("engine.ai", e.msg)),
            }
        })
    }

    fn call_tool<'a>(&'a self, name: String, args: Value, _ctx: ToolCtx, _cancel: Cancel) -> BoxFut<'a, Result<Value, NodeError>> {
        Box::pin(async move {
            let Some(h) = self.hub.upgrade() else { return Err(NodeError::fatal("engine.unavailable", "the hub is stopping")) };
            let ai: &dyn AiHub = &*h;
            let out = if let Some(op) = pgm_collab_internal::board::tool_op(&name) { ai.board_tool(op, &self.user, &args) }
                else if name.starts_with("notes_") { ai.notes_tool(&name, &args, &h.mods.name_of(&self.user)) }
                else { return Err(NodeError::fatal("engine.tool-denied", format!("the hub has no tool `{name}` for workflows"))) };
            if out["ok"] == json!(false) { return Err(NodeError::new("engine.tool", out["error"].as_str().unwrap_or("the tool failed").to_string())); }
            Ok(out.get("result").cloned().unwrap_or(out))
        })
    }

    fn http<'a>(&'a self, req: HttpRequest, _cancel: Cancel) -> BoxFut<'a, Result<HttpAnswer, NodeError>> {
        Box::pin(async move {
            let Some(h) = self.hub.upgrade() else { return Err(NodeError::fatal("engine.unavailable", "the hub is stopping")) };
            let host = req.url.split("://").nth(1).and_then(|r| r.split(['/', ':', '?']).next()).unwrap_or("").to_lowercase();
            let allowed = h.cfg.get("hyperworkNetwork").and_then(Value::as_array).is_some_and(|a| a.iter().filter_map(Value::as_str).any(|x| x.eq_ignore_ascii_case(&host)));
            if !allowed { return Err(NodeError::fatal("engine.denied", format!("this hub does not let workflows call {host} (config hyperworkNetwork)"))); }
            let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(60)).build().map_err(|e| NodeError::new("engine.http", e.to_string()))?;
            let method = reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|_| NodeError::fatal("engine.config", format!("`{}` is not an HTTP method", req.method)))?;
            let mut rb = client.request(method, &req.url);
            for (k, v) in &req.headers { rb = rb.header(k, v); }
            if let Some(b) = req.body { rb = rb.body(b); }
            let resp = rb.send().await.map_err(|e| NodeError::new("engine.http", e.to_string()))?;
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Ok(HttpAnswer { status, body: body.chars().take(1_000_000).collect() })
        })
    }

    fn ask_person(&self, ask: &pgm_workflow_engine_internal::Ask) -> Result<String, NodeError> { Ok(format!("hq-{}-{}", ask.run, ask.key)) }

    /// The hub's workflow secrets (config `hyperworkSecrets`, on the hub only; never in a document or a log).
    fn secret(&self, name: &str) -> Option<String> { self.hub.upgrade()?.cfg.get("hyperworkSecrets")?.get(name)?.as_str().map(str::to_string) }
}
