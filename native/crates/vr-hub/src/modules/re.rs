//! RE containers (T-303): the hub's routes (`/api/re/…`) and agent tools (`re_*`) over pgm-sandbox-internal.
//! Every copy is a throw-away clone of the pre-set-up base image (config `re: { image, maxConcurrent: 3, idleWarnHours,
//! runtime, … }`); anyone on the team may start one, no approval; only the owner (or an admin, to stop) controls it.
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pgm_sandbox_internal::{Actor, Cli, Cmd, Config as SbConfig, Error, Manager, StartOpts, State};

use super::auth::Account;
use super::Config;
use super::Routed;
use crate::front::{json as jresp, Hub, Req};

pub struct Re {
    pub mgr: Arc<Manager>,
    ready: Mutex<Option<(Instant, Result<(), String>)>>,
    reconciled: std::sync::atomic::AtomicBool,
}

fn answer(status: u16, v: Value) -> Routed { Routed::Answer(jresp(status, &v)) }

impl Re {
    pub fn new(mills: &Path, cfg: &Config) -> Re {
        let c = cfg.get("re").cloned().unwrap_or(json!({}));
        let root = c["exchangeDir"].as_str().filter(|s| !s.is_empty()).map(std::path::PathBuf::from).unwrap_or_else(|| mills.join("Re"));
        let env: Vec<(String, String)> = c["env"].as_object().map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect()).unwrap_or_default();
        let eargs: Vec<String> = c["engineArgs"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let cli = Cli::named(c["runtime"].as_str().unwrap_or("auto")).with_env(env).with_args(eargs);
        Re { mgr: Arc::new(Manager::new(Box::new(cli), SbConfig::from_json(root, &c))), ready: Mutex::new(None), reconciled: Default::default() }
    }

    /// Whether the engine and the base image are there; a good answer is kept 30 s, a bad one is asked again.
    fn ready(&self) -> Result<(), String> {
        let mut g = self.ready.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, Ok(()))) = &*g { if t.elapsed() < Duration::from_secs(30) { return Ok(()); } }
        let r = self.mgr.ready();
        *g = Some((Instant::now(), r.clone()));
        r
    }

    /// One operation for a person (a route or an agent tool): `{ … }` or the refusal.
    pub fn op(&self, name: &str, a: &Value, who: &Actor) -> Result<Value, Error> {
        let id = a["id"].as_str().unwrap_or("").to_string();
        let m = &self.mgr;
        match name {
            "start" => {
                self.ready().map_err(|e| Error::new(503, e))?;
                let s = m.start(who, a["purpose"].as_str().unwrap_or(""), &StartOpts { network: a["network"].as_bool() == Some(true), image: None })?;
                if s.state == State::Gone { return Err(Error::new(502, s.error.unwrap_or_else(|| "The copy did not start.".into()))); }
                Ok(json!({ "sandbox": s, "hint": if s.state == State::Queued { "Queued: poll re_status until its state is running." } else { "Running. REA (reverse engineering agent) is inside it: call re_skill now (its workflow, read it before you start), then re_tools (its tools; filter to search) and re_call to use them. Samples go in with re_put, shell commands with re_exec, results out with re_get; re_stop when you are done (it is deleted entirely)." } }))
            }
            "list" => {
                let (active, queued, max) = m.counts();
                let orphans = m.orphans();
                Ok(json!({ "sandboxes": m.list(), "running": active, "queued": queued, "max": max, "image": m.config().image, "runtime": m.runtime_name(), "idleWarnHours": m.config().idle_warn_hours, "you": who.user,
                    "ready": self.ready().err().map(|e| json!({ "ok": false, "error": e })).unwrap_or(json!({ "ok": true })),
                    "orphans": if who.admin { json!(orphans) } else { json!(orphans.len()) } }))
            }
            "status" => { let s = m.status(&id)?; let files = m.files(&id); Ok(json!({ "sandbox": s, "files": files })) }
            "exec" => {
                let Some(cmd) = Cmd::from_json(&a["cmd"]) else { return Err(Error::new(400, "cmd is a command line (string) or a list of arguments.")) };
                Ok(json!(m.exec(who, &id, &cmd, a["timeoutSec"].as_u64())?))
            }
            "put" => {
                let bytes = if let Some(b) = a["base64"].as_str() { pgm_core_internal::unb64(b).map_err(|_| Error::new(400, "base64 is not valid."))? } else if let Some(t) = a["text"].as_str() { t.as_bytes().to_vec() } else { return Err(Error::new(400, "Give text or base64.")) };
                let (path, size) = m.put(who, &id, a["path"].as_str().unwrap_or(""), &bytes)?;
                Ok(json!({ "path": path, "size": size }))
            }
            "get" => {
                let bytes = m.get(who, &id, a["path"].as_str().unwrap_or(""))?;
                let mut v = json!({ "path": a["path"], "size": bytes.len() });
                if bytes.len() <= (256 << 10) { if let Ok(t) = std::str::from_utf8(&bytes) { v["text"] = json!(t); } }
                if bytes.len() > (2 << 20) { return Err(Error::new(413, format!("That file is {} bytes: too big for a tool result; the person can download it from GET /api/re/get.", bytes.len()))); }
                if v.get("text").is_none() { v["base64"] = json!(pgm_core_internal::b64(&bytes)); }
                Ok(v)
            }
            // T-254: REA's MCP server inside the copy (one session kept per copy, ended with the copy)
            "skill" => m.skill(who, &id, a["file"].as_str().unwrap_or("")),
            "tools" => m.mcp_tools(who, &id, a["filter"].as_str().unwrap_or(""), a["tool"].as_str().unwrap_or("")),
            "call" => m.mcp_call(who, &id, a["tool"].as_str().unwrap_or(""), &a["args"], a["timeoutSec"].as_u64()),
            "stop" => Ok(json!({ "sandbox": m.stop(who, &id)?, "deleted": true })),
            _ => Err(Error::new(404, "Unknown endpoint")),
        }
    }

    /// An agent tool (`re_start` … `re_stop`) for a person: `{ ok, result }` / `{ ok: false, error }`.
    pub async fn tool(self: &Arc<Self>, name: &str, args: &Value, a: &Account) -> Value { self.tool_for(name, args, Actor::new(&a.username, a.admin)).await }
    /// The same for a workflow run (T-255): as its owner, with no admin rights whoever the owner is.
    pub async fn tool_as(self: &Arc<Self>, name: &str, args: &Value, user: &str) -> Value { self.tool_for(name, args, Actor::new(user, false)).await }
    async fn tool_for(self: &Arc<Self>, name: &str, args: &Value, who: Actor) -> Value {
        let Some(op) = name.strip_prefix("re_").filter(|o| matches!(*o, "start" | "list" | "status" | "exec" | "put" | "get" | "stop" | "skill" | "tools" | "call")) else { return json!({ "ok": false, "error": format!("Unknown team tool: {name}") }) };
        let (me, op, args) = (self.clone(), op.to_string(), args.clone());
        match tokio::task::spawn_blocking(move || me.op(&op, &args, &who)).await {
            Ok(Ok(v)) => json!({ "ok": true, "result": v }),
            Ok(Err(e)) => json!({ "ok": false, "error": e.msg }),
            Err(_) => json!({ "ok": false, "error": "The RE container manager failed (a panic inside it); it was logged." }),
        }
    }
}

/// `/api/re/…` for a signed-in person (everyone on the team; no approval).
pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    let rest = r.path.strip_prefix("/api/re/")?;
    let re = hub.mods.re.clone();
    let who = Actor::new(&a.username, a.admin);
    let get = r.method == hyper::Method::GET; let post = r.method == hyper::Method::POST || r.method == hyper::Method::PUT;
    let err = |e: Error| answer(e.status, json!({ "error": e.msg }));
    let blocking = |f: Box<dyn FnOnce() -> Result<Value, Error> + Send>| async move { match tokio::task::spawn_blocking(f).await { Ok(Ok(v)) => answer(200, v), Ok(Err(e)) => answer(e.status, json!({ "error": e.msg })), Err(_) => answer(500, json!({ "error": "The RE container manager failed; it was logged." })) } };
    Some(match rest {
        "list" | "status" if get => {
            let args = json!({ "id": r.param("id").unwrap_or_default() });
            let n = rest.to_string();
            blocking(Box::new(move || re.op(&n, &args, &who))).await
        }
        "skill" | "tools" if get => {
            let args = json!({ "id": r.param("id").unwrap_or_default(), "file": r.param("file").unwrap_or_default(), "filter": r.param("filter").unwrap_or_default(), "tool": r.param("tool").unwrap_or_default() });
            let n = rest.to_string();
            blocking(Box::new(move || re.op(&n, &args, &who))).await
        }
        "start" | "exec" | "stop" | "call" if post => { let (n, b) = (rest.to_string(), r.json()); blocking(Box::new(move || re.op(&n, &b, &who))).await }
        // a file in: JSON { id, path, text | base64 }, or the raw body with ?id=&path=
        "put" if post => {
            if let (Some(id), Some(path)) = (r.param("id"), r.param("path")) {
                let body = r.body.to_vec();
                blocking(Box::new(move || { let (p, size) = re.mgr.put(&who, &id, &path, &body)?; Ok(json!({ "path": p, "size": size })) })).await
            } else { let b = r.json(); blocking(Box::new(move || re.op("put", &b, &who))).await }
        }
        // a file out: raw bytes with ?id=&path= (what a person downloads), or JSON { id, path } for text / base64
        "get" if get => {
            let (id, path) = (r.param("id").unwrap_or_default(), r.param("path").unwrap_or_default());
            let name = path.rsplit('/').next().unwrap_or("file").to_string();
            let res = tokio::task::spawn_blocking(move || re.mgr.get(&who, &id, &path)).await;
            match res { Ok(Ok(buf)) => super::serve_file(buf, &name, "application/octet-stream"), Ok(Err(e)) => err(e), Err(_) => answer(500, json!({ "error": "The RE container manager failed; it was logged." })) }
        }
        "get" if post => { let b = r.json(); blocking(Box::new(move || re.op("get", &b, &who))).await }
        _ => answer(404, json!({ "error": "Unknown endpoint" })),
    })
}

/// Adopt the copies of the last run (and list orphans) once the engine answers.
fn reconcile(re: &Re) {
    use std::sync::atomic::Ordering;
    if re.reconciled.load(Ordering::SeqCst) { return; }
    match re.mgr.reconcile() {
        Ok((adopted, orphans)) => { re.reconciled.store(true, Ordering::SeqCst); if adopted + orphans > 0 { println!("[vr-hub] RE containers: adopted {adopted} running copies, {orphans} orphans listed (never touched until an admin clears them)."); } }
        Err(e) => println!("[vr-hub] RE containers: could not reconcile yet ({}); trying again every 30 s.", e.msg),
    }
}

/// Start-up: reconcile, then the housekeeping tick every 30 s.
pub fn start(hub: &Arc<Hub>) {
    let r1 = hub.mods.re.clone();
    tokio::task::spawn_blocking(move || reconcile(&r1));
    let h = hub.clone();
    super::forever("RE containers tick", move || { let h = h.clone(); async move {
        loop { tokio::time::sleep(Duration::from_secs(30)).await; let re = h.mods.re.clone(); let _ = tokio::task::spawn_blocking(move || { reconcile(&re); re.mgr.tick(); }).await; }
    } });
}
