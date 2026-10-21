//! Work (moved from bridge/server.js runClaude and its neighbours): each person's Claude Code runs in their own git
//! worktree, the stream of its events to their page, the PC tools relayed to their own AI host or their browser, the
//! board / notes / team tools of a run, a host's shared login relayed for a run here (/llm/<token>/…), and around it:
//! the model and provider choice, the project, usage (/api/usage), shared Work sessions (watch-only), the Work
//! conversation + queue kept across restarts (work-current.json), and past sessions (Archive/).
//! State per person: <profile>/work.json { provider, model, claudeSessionId, claudeSessionWhere, ctx, project,
//! lastRunAt }, taken over from their state.json the first time (the JavaScript hub keeps the rest of state.json).
//! Providers are read here (bridge/providers.json, still edited in Settings by the JavaScript hub) plus the AI hosts'.
use bytes::Bytes;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Notify};

use super::auth::Account;
use super::worktree::{Worktrees, Wt, HUB_PROJECT};
use super::{answer, Config, Routed};
use crate::front::{Hub, Req};
use vr_core::now_ms;

const AI_OFF_TEXT: &str = "The host's AI is switched off right now (its host paused it). Chat, the board and your own AI keep working; pick your own AI in the model dropdown, or try again once it is back.";
const STATE_KEYS: [&str; 7] = ["provider", "model", "claudeSessionId", "claudeSessionWhere", "ctx", "project", "lastRunAt"];

struct Run { id: String, tx: mpsc::UnboundedSender<Result<Bytes, String>>, pid: Option<u32>, remote: String, pending: HashMap<String, oneshot::Sender<Value>> }
struct Share { conv: String, since: String, n: u64, log: Vec<Value>, live: Value, ver: u64, woke: Arc<Notify> }
#[derive(Clone, Default)]
struct Fresh { text: String, at: u64, busy: bool }

pub struct Work {
    root: PathBuf,
    mills: PathBuf,
    cfg: Config,
    /// the MCP relays of runs here (x-mills-internal)
    pub secret: String,
    pub wt: Worktrees,
    states: Mutex<HashMap<String, Value>>,
    runs: Mutex<HashMap<String, Run>>,
    llm: Mutex<HashMap<String, (String, String, String, u64)>>,
    fresh: Mutex<HashMap<String, Fresh>>,
    shares: Mutex<HashMap<String, Share>>,
    projects: Mutex<(u64, Value)>,
    /// the shared sessions changed (the JavaScript hub's room poll lists them)
    pub shares_changed: Notify,
    /// a run started or ended (the JavaScript hub restarts only while nobody's Claude is busy, here included)
    pub busy_changed: Notify,
}
fn read_json(p: &Path) -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) }
fn write_json(p: &Path, v: &Value) { let tmp = p.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp, p); } }
fn line(v: Value) -> Result<Bytes, String> { Ok(Bytes::from(v.to_string() + "\n")) }
fn s(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_string() }
fn iso() -> String { super::auth::iso_now() }

impl Work {
    pub fn new(root: &Path, cfg: &Config) -> Work {
        Work { root: root.to_path_buf(), mills: root.join(".."), cfg: cfg.clone(), secret: vr_core::crypto::random_hex(24), wt: Worktrees::new(root, cfg), states: Mutex::default(), runs: Mutex::default(), llm: Mutex::default(), fresh: Mutex::default(), shares: Mutex::default(), projects: Mutex::new((0, json!([]))), shares_changed: Notify::new(), busy_changed: Notify::new() }
    }
    // ---------- each person's Work state ----------
    fn state_file(a: &Account) -> PathBuf { a.dir.join("work.json") }
    /// Their Work state (work.json; the first time, what their state.json held).
    pub fn state(&self, a: &Account) -> Value {
        if let Some(v) = self.states.lock().unwrap().get(&a.username) { return v.clone(); }
        let v = read_json(&Self::state_file(a)).filter(Value::is_object).unwrap_or_else(|| {
            let old = read_json(&a.dir.join("state.json")).unwrap_or(json!({}));
            let mut m = Map::new(); for k in STATE_KEYS { if !old[k].is_null() { m.insert(k.into(), old[k].clone()); } }
            let v = Value::Object(m); write_json(&Self::state_file(a), &v); v
        });
        self.states.lock().unwrap().insert(a.username.clone(), v.clone());
        v
    }
    fn update(&self, a: &Account, f: impl FnOnce(&mut Map<String, Value>)) -> Value {
        let mut v = self.state(a); if !v.is_object() { v = json!({}); }
        f(v.as_object_mut().unwrap());
        write_json(&Self::state_file(a), &v);
        self.states.lock().unwrap().insert(a.username.clone(), v.clone());
        v
    }
    pub fn forget(&self, u: &str) { self.states.lock().unwrap().remove(u); }
    pub fn drop_provider(&self, a: &Account, id: &str) { if self.state(a)["provider"] == json!(id) { self.update(a, |m| { m.remove("provider"); }); } }
    pub fn busy(&self, u: &str) -> bool { self.runs.lock().unwrap().contains_key(u) }
    pub fn any_busy(&self) -> bool { !self.runs.lock().unwrap().is_empty() }
    pub fn default_model_of(&self, a: &Account) -> String { self.default_model(a) }
    fn default_model(&self, a: &Account) -> String {
        let c = if a.dir == self.root { Value::Object(self.cfg.raw.clone()) } else { read_json(&a.dir.join("config.json")).unwrap_or(Value::Null) };
        s(&c, "model")
    }
    pub fn effective_model(&self, a: &Account) -> String { let m = s(&self.state(a), "model"); if m.is_empty() { self.default_model(a) } else { m } }
    pub fn project_of(&self, a: &Account) -> String { let p = s(&self.state(a), "project"); self.wt.project(&p).map(|p| p.name).unwrap_or_else(|_| HUB_PROJECT.into()) }
    pub fn project_list(&self) -> Value {
        let mut c = self.projects.lock().unwrap();
        if now_ms() - c.0 > 5000 { *c = (now_ms(), json!(self.wt.list())); }
        c.1.clone()
    }
    pub fn projects_changed(&self) { self.projects.lock().unwrap().0 = 0; }
    fn model_ids(&self) -> Vec<String> { self.cfg.get("models").and_then(Value::as_array).map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default() }

    // ---------- providers (bridge/providers.js, read here) ----------
    fn hub_claude(&self) -> bool { self.cfg.get("hubClaude") != Some(&json!(false)) }
    fn host_entry(&self) -> Value { json!({ "id": "host", "kind": "host", "label": "Host Claude (default)", "owner": "", "shared": true, "models": self.model_ids() }) }
    fn own_providers(&self) -> Vec<Value> { read_json(&self.root.join("providers.json")).and_then(|v| v["providers"].as_array().cloned()).unwrap_or_default() }
    pub fn provider_get(&self, ai: &super::ai_hosts::AiHosts, id: &str, user: &str) -> Option<Value> {
        if id.is_empty() || id == "host" { return if self.hub_claude() { Some(self.host_entry()) } else { None }; }
        if id.starts_with("h:") { return ai.provider(id, user); }
        self.own_providers().into_iter().find(|p| p["id"] == json!(id)).filter(|p| user.is_empty() || p["shared"] == json!(true) || p["owner"] == json!(user))
    }
    /// Config defaultProvider "host:<user>" → that person's online Claude host, else this hub's own Claude, else the
    /// first Claude a host offers them.
    pub fn default_for(&self, ai: &super::ai_hosts::AiHosts, user: &str) -> Option<Value> {
        let claudes: Vec<Value> = ai.providers_for(user).into_iter().filter(|p| p["kind"] == json!("host-claude")).collect();
        let usable = |p: &&Value| p["online"] == json!(true) && p["capped"].as_str().unwrap_or("").is_empty();
        if let Some(w) = self.cfg.get("defaultProvider").and_then(Value::as_str).and_then(|d| d.strip_prefix("host:")) {
            if let Some(p) = claudes.iter().filter(|x| x["owner"] == json!(w)).find(usable).or_else(|| claudes.iter().find(|x| x["owner"] == json!(w))) { return Some(p.clone()); }
        }
        if self.hub_claude() { return Some(self.host_entry()); }
        claudes.iter().find(usable).or(claudes.first()).cloned()
    }
    pub fn effective(&self, ai: &super::ai_hosts::AiHosts, user: &str, chosen: &str) -> Option<Value> { (if chosen.is_empty() { None } else { self.provider_get(ai, chosen, user) }).or_else(|| self.default_for(ai, user)) }
    /// What a run's `claude` gets for that provider (None: not Claude, it cannot run Work).
    fn claude_env(p: &Value) -> Option<Vec<(String, String)>> {
        match p["kind"].as_str().unwrap_or("") { "host" | "host-claude" => Some(vec![]), "anthropic" => Some(vec![("ANTHROPIC_API_KEY".into(), s(p, "apiKey"))]), "claude-cli" => Some(vec![("CLAUDE_CONFIG_DIR".into(), s(p, "configDir"))]), _ => None }
    }
    fn usage_source(&self, ai: &super::ai_hosts::AiHosts, a: &Account) -> String { match self.effective(ai, &a.username, &s(&self.state(a), "provider")) { Some(p) if p["kind"] == json!("host-claude") => s(&p, "id"), _ => "hub".into() } }
    pub fn usage_status(&self, hub: &Hub, a: &Account) -> Value { let st = self.state(a); hub.mods.ai.usage.status(&self.usage_source(&hub.mods.ai, a), &st["ctx"], st["claudeSessionId"].as_str().unwrap_or("")) }

    // ---------- the person's Forgejo token (bridge/forgejo.js keeps it) ----------
    fn forgejo_url(&self) -> String { read_json(&self.root.join("forgejo.json")).and_then(|c| c["url"].as_str().map(String::from)).or_else(|| self.cfg.get("forgejoUrl").and_then(Value::as_str).map(String::from)).unwrap_or_else(|| "https://git.unityailab.com".into()).trim_end_matches('/').to_string() }
    fn forgejo_rec(&self, u: &str) -> Option<Value> { read_json(&self.root.join("forgejo-users.json")).and_then(|v| v["users"].get(u).cloned()).filter(Value::is_object) }
    pub fn work_env(&self, u: &str) -> Vec<(String, String)> {
        let Some(r) = self.forgejo_rec(u) else { return vec![] };
        use base64::Engine;
        let url = self.forgejo_url(); let n: u64 = std::env::var("GIT_CONFIG_COUNT").ok().and_then(|x| x.parse().ok()).unwrap_or(0);
        vec![("FORGEJO_URL".into(), url.clone()), ("FORGEJO_TOKEN".into(), s(&r, "token")), ("FORGEJO_LOGIN".into(), s(&r, "login")), ("GIT_CONFIG_COUNT".into(), (n + 1).to_string()),
            (format!("GIT_CONFIG_KEY_{n}"), format!("http.{url}/.extraHeader")), (format!("GIT_CONFIG_VALUE_{n}"), format!("Authorization: Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", s(&r, "login"), s(&r, "token")))))]
    }

    // ---------- worktree freshness (the worktree law): checked in the background, never holding a run up ----------
    pub fn check_fresh(self: &Arc<Self>, a: &Account) {
        let pj = self.project_of(a); let key = format!("{}|{pj}", a.username);
        { let mut f = self.fresh.lock().unwrap(); let cur = f.get(&key).cloned().unwrap_or_default(); if cur.busy || (cur.at > 0 && now_ms() - cur.at < 60000) { return; } f.insert(key.clone(), Fresh { busy: true, ..cur }); }
        let (w, user) = (self.clone(), a.username.clone());
        tokio::task::spawn_blocking(move || {
            let mut env = w.work_env(&user); env.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
            let (_, out, err) = w.wt.tool(&["--project", &pj, "auto", &user], &env);
            let raw = if out.trim().is_empty() { err } else { out };
            let lines: Vec<&str> = raw.trim().lines().collect();
            let text: String = lines[lines.len().saturating_sub(2)..].join(" ").chars().take(600).collect();
            w.fresh.lock().unwrap().insert(key, Fresh { text, at: now_ms(), busy: false });
        });
    }
    fn fresh_law(&self, a: &Account, wd: &Wt) -> String {
        let script = self.wt.script.display().to_string().replace('\\', "/");
        let cmd = if wd.hub { "node bridge/worktree.js".to_string() } else { format!("node {script} --project {}", wd.project) };
        let u = &a.username;
        let f = self.fresh.lock().unwrap().get(&format!("{u}|{}", wd.project)).cloned();
        format!("Worktree law: never build on a stale worktree. Before changing anything, check it: {cmd} check {u} (it fetches Forgejo first; exit code 3 = stale). If it is STALE, run {cmd} update {u}: it fast-forwards, merges Forgejo's {} into your own commits, or makes a NEW worktree from Forgejo with your commits, uncommitted changes and untracked files carried over and then deletes just the old one ({cmd} renew {u} does that directly).", wd.main)
            + &f.filter(|f| !f.text.is_empty()).map(|f| format!(" Latest background check ({} min ago): {}", ((now_ms() - f.at) as f64 / 60000.0).round().max(1.0), f.text)).unwrap_or_default()
    }
    fn hub_laws(&self) -> String { ["LAWS.md", "CLAUDE.md"].iter().filter_map(|f| std::fs::read_to_string(self.mills.join(f)).ok().map(|t| t.trim().to_string())).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n") }
    fn system_prompt(&self, a: &Account, wd: &Wt) -> String {
        let (name, u) = (&a.display, &a.username);
        let script = self.wt.script.display().to_string().replace('\\', "/");
        let dir = wd.dir.display(); let b = &wd.branch;
        let wher: Vec<String> = if wd.hub { vec![
            format!("For work on the VibeRoller project itself you are in {name}'s OWN git worktree: {dir}, branch {b}."),
            format!("Commit only on {b}; never check out other branches here and never touch other people's worktrees in {}.", self.wt.wt_root.display()),
            "Before committing, run the regression suite from the worktree: node bridge/test/regress.js --no-prod (it must report 0 failed).".into(),
            format!("Only when {name} asks you to merge into develop, run from the worktree: node bridge/worktree.js commit {u} \"<message>\" then node bridge/worktree.js merge {u}"),
            format!("(it merges onto Forgejo's develop and pushes to Forgejo first; on a conflict run \"node bridge/worktree.js update {u}\", resolve, commit, merge again). Going live (node bridge/worktree.js live: Forgejo's develop → main, pushed first; the server's auto-deploy ships it, health-checks it and rolls back by itself) only when asked."),
            self.fresh_law(a, wd),
            format!("Other projects live in the shared projects area {} (one folder per project, _repo = its main checkout, one worktree per person); {name} switches projects in Work's header. \"node bridge/worktree.js projects\" lists them.", self.wt.projects_root.display()),
        ] } else { let pj = &wd.project; vec![
            format!("You are working on the project \"{pj}\" (not on VibeRoller) in {name}'s OWN git worktree: {dir}, branch {b}. The project's main checkout is {} (integration branch {}); other people's worktrees sit next to yours in {}.", wd.repo.display(), wd.main, wd.root.display()),
            format!("Commit only on {b}; never check out other branches here and never touch the main checkout or other people's worktrees."),
            format!("Only when {name} asks you to merge, run: node {script} --project {pj} commit {u} \"<message>\" then node {script} --project {pj} merge {u}"),
            format!("(that merges into {} and pushes it when the project has a remote; on a conflict run \"node {script} --project {pj} sync {u}\", resolve, commit, merge again). The hub's own tooling (regression suite, live) is for VibeRoller only.", wd.main),
            self.fresh_law(a, wd),
        ] };
        let mut parts = vec![
            format!("You are talking with {name} (username \"{u}\"{}, Unity AI Lab engineer) through the VibeRoller web interface, running on the host's Claude account.", if a.admin { ", admin" } else { "" }),
            format!("Every message in this conversation comes from {name}; address them by name and keep their work separate from everyone else's."),
        ];
        parts.extend(wher);
        parts.extend([
            format!("{name} has opened a folder on THEIR OWN PC in their browser. To build or edit anything on their PC, use the mcp__pc__* tools"),
            format!("(list, read, write, delete, mkdir) with paths relative to that folder. Your built-in file tools act on the host's VibeRoller folder, not {name}'s PC."),
            format!("Shell commands cannot run on {name}'s PC; if something needs running there, tell {name} exactly what to run."),
            format!("The VibeRoller team board (shared with everyone and with other Claude runs) is available through the mcp__board__* tools, where you act as \"{name} · Claude\"."),
            "When you work a board task: task_claim it first, task_renew while working longer than the claim, add progress with task_update appendNote, and task_complete or task_release it when you stop. Never edit a task someone else holds.".into(),
            "You may plan with the board and calendar when asked: board_calendar shows tasks by due day, task_create / task_update take due as text (today, tomorrow, +3d, next friday, YYYY-MM-DD, none), task_move puts a task in a column, task_delete removes one (only when asked), board_new makes a board. A task with a due date IS the calendar entry.".into(),
            format!("Beyond chat: hub_status (who is here, calls, shared Work sessions, the host's usage windows), sessions_search ({name}'s past Claude sessions), my_providers (the AIs available), team_channel_create / team_group_create."),
        ]);
        if self.forgejo_rec(u).map(|r| !s(&r, "token").is_empty()).unwrap_or(false) { parts.push(format!("Forgejo ({}): {name} saved their own token, so git pushes / fetches to that host authenticate as them, and FORGEJO_URL / FORGEJO_TOKEN are in your environment for its API (never print the token).", self.forgejo_url())); }
        parts.push("Team notes (markdown files in folders, shared by everyone): notes_list (with a search), notes_read, notes_write (path like \"plans/next-week.md\"; whole text), notes_delete (only when asked). Good for plans, meeting notes, decisions, how-tos.".into());
        parts.push(format!("The team chat is reachable through the mcp__team__* tools: team_say posts a message to # Everyone (or privately with to=<username>) as \"{name} · Claude\", team_presence tells you who is online. Announce in the chat BEFORE any hub restart and when you finish something the team should know about."));
        let laws = self.hub_laws();
        parts.join(" ") + &if laws.is_empty() { String::new() } else { format!("\n\n{laws}") }
    }
    /// The MCP servers of a run here: PC files, the board, the team chat (bridge/*-mcp.js), with this hub's secret.
    fn mcp_config(&self, a: &Account) -> PathBuf {
        let f = a.dir.join(".mcp-runtime-hub.json");
        let node = std::env::var("VR_NODE").unwrap_or_else(|_| "node".into());
        let env = json!({ "MILLS_BRIDGE": format!("http://127.0.0.1:{}", self.cfg.port), "MILLS_INTERNAL": self.secret, "MILLS_USER": a.display, "MILLS_ACCOUNT": a.username });
        let srv = |js: &str| json!({ "command": node, "args": [std::path::absolute(self.root.join(js)).unwrap_or_default().display().to_string()], "env": env });
        write_json(&f, &json!({ "mcpServers": { "pc": srv("pc-tools-mcp.js"), "board": srv("board-mcp.js"), "team": srv("team-mcp.js") } }));
        vr_core::settings::private(&f);
        f
    }
    fn repo_url_for(&self, wd: &Wt) -> String {
        if wd.hub { return self.cfg.get("hubRepoUrl").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| super::worktree::git(&self.mills, &["remote", "get-url", "origin"]).unwrap_or_default()); }
        self.cfg.get("projectsGitBase").and_then(Value::as_str).map(|b| format!("{}/{}/_repo", b.trim_end_matches('/'), wd.project)).unwrap_or_default()
    }
    // ---------- the login relay: one token per person + host provider, 12 hours, from this machine only ----------
    fn llm_token(&self, user: &str, host: &str, prov: &str) -> String {
        let mut m = self.llm.lock().unwrap(); let now = now_ms();
        m.retain(|_, v| v.3 > now);
        if let Some((t, _)) = m.iter().find(|(_, v)| v.0 == user && v.1 == host && v.2 == prov && v.3 - now > 3_600_000) { return t.clone(); }
        let t = vr_core::crypto::random_hex(24);
        m.insert(t.clone(), (user.into(), host.into(), prov.into(), now + 12 * 3_600_000));
        t
    }

    // ---------- the run's stream ----------
    fn emit(&self, u: &str, ev: Value) { if let Some(r) = self.runs.lock().unwrap().get(u) { let _ = r.tx.send(line(ev)); } }
    fn on_event(&self, hub: &Hub, a: &Account, ev: Value, source: &str) {
        if let Some(sid) = ev["session_id"].as_str().filter(|s| !s.is_empty()) { let sid = sid.to_string(); self.update(a, |m| { m.insert("claudeSessionId".into(), json!(sid)); }); }
        let usage = &hub.mods.ai.usage;
        let t = ev["type"].as_str().unwrap_or("");
        if !source.is_empty() { usage.note_event(&ev, source); } else if !t.contains("rate_limit") { usage.note_event(&ev, "hub"); }
        if t == "assistant" || t == "result" {
            let st = self.state(a); let mut ctx = st["ctx"].clone(); let before = ctx.clone();
            super::usage::note_context(&mut ctx, &ev, st["claudeSessionId"].as_str().unwrap_or(""));
            if ctx != before { self.update(a, |m| { m.insert("ctx".into(), ctx); }); }
        }
        self.share_event(&a.username, &ev);
        self.emit(&a.username, ev);
    }
    fn finish(&self, a: &Account, run_id: &str, code: Value) {
        let run = { let mut runs = self.runs.lock().unwrap(); if runs.get(&a.username).map(|r| r.id == run_id).unwrap_or(false) { runs.remove(&a.username) } else { None } };
        let Some(r) = run else { return };
        for (_, p) in r.pending { let _ = p.send(json!({ "ok": false, "error": "Run ended" })); }
        let _ = r.tx.send(line(json!({ "type": "bridge_done", "code": code })));
        self.busy_changed.notify_one();
    }
    /// Stops the person's run: the process here, or the host is told.
    pub fn stop(&self, hub: &Hub, u: &str) {
        let (pid, remote) = match self.runs.lock().unwrap().get(u) { Some(r) => (r.pid, r.remote.clone()), None => return };
        if !remote.is_empty() { hub.mods.ai.cancel_work(&remote); return; }
        if let Some(pid) = pid { kill_pid(pid); }
    }
}
#[cfg(unix)]
pub fn kill_pid(pid: u32) { unsafe { libc::kill(pid as i32, libc::SIGTERM); } }
#[cfg(not(unix))]
pub fn kill_pid(pid: u32) { let _ = std::process::Command::new("taskkill").args(["/pid", &pid.to_string(), "/T", "/F"]).spawn(); }

fn ndjson(rx: mpsc::UnboundedReceiver<Result<Bytes, String>>) -> hyper::Response<crate::front::Body> {
    use http_body_util::BodyExt;
    let st = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x.map(hyper::body::Frame::data).map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e)), rx)) });
    hyper::Response::builder().status(200).header("content-type", "application/x-ndjson").header("cache-control", "no-cache").header("x-accel-buffering", "no").body(http_body_util::StreamBody::new(st).boxed()).expect("response")
}
fn one_line(v: Value) -> Routed { Routed::Answer(hyper::Response::builder().status(200).header("content-type", "application/x-ndjson").body(crate::front::full(v.to_string() + "\n")).expect("response")) }

/// POST /api/chat: one message to Claude in Work.
async fn chat(hub: &Arc<Hub>, a: &Account, r: &Req) -> Routed {
    let w = &hub.mods.work;
    if w.busy(&a.username) { return answer(409, json!({ "error": "Claude is already working. Stop it first." })); }
    // a requested restart wins the gap between two replies (the page retries a 503 by itself)
    if std::env::var("MILLS_SUPERVISED").is_ok() && hub.root.join(".restart").exists() { return answer(503, json!({ "error": "VibeRoller is restarting; your message goes through in a moment." })); }
    let b = r.json();
    let Some(message) = b["message"].as_str().filter(|m| !m.is_empty()).map(String::from) else { return answer(400, json!({ "error": "Empty message." })) };
    let context: String = b["context"].as_str().unwrap_or("").chars().take(20000).collect();
    let fresh = b["fresh"].as_bool().unwrap_or(false) || (b["fresh"].is_number() && b["fresh"] != json!(0)) || b["fresh"].as_str().map(|s| !s.is_empty()).unwrap_or(false);
    if fresh { w.update(a, |m| { m.remove("claudeSessionId"); m.remove("ctx"); }); }
    w.share_line(&a.username, "user", &message);
    let pj = w.project_of(a);
    let wd = match w.wt.ensure(&a.username, &pj) { Ok(wd) => wd, Err(e) => return one_line(json!({ "type": "bridge_error", "text": format!("Could not prepare your worktree: {e} (switch to the shared folder or ask the host).") })) };
    w.update(a, |m| { m.insert("lastRunAt".into(), json!(now_ms())); });
    w.check_fresh(a);
    let st = w.state(a);
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose", "--permission-mode"].iter().map(|x| x.to_string()).collect();
    args.push(hub.cfg.get("permissionMode").and_then(Value::as_str).unwrap_or("default").into());
    args.extend(["--mcp-config".into(), w.mcp_config(a).display().to_string(), "--strict-mcp-config".into(), "--append-system-prompt".into(), w.system_prompt(a, &wd)]);
    let mut tools: Vec<String> = hub.cfg.get("allowedTools").and_then(Value::as_array).map(|t| t.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if !wd.hub { tools.push(format!("Bash(node {}:*)", w.wt.script.display().to_string().replace('\\', "/"))); }
    if !tools.is_empty() { args.push("--allowedTools".into()); args.push(tools.join(",")); }
    let model = w.effective_model(a);
    if !model.is_empty() { args.push("--model".into()); args.push(model.clone()); }
    let ai = &hub.mods.ai;
    let Some(prov) = w.effective(ai, &a.username, &s(&st, "provider")) else { return one_line(json!({ "type": "bridge_error", "text": "No AI is available for Work: this hub has no Claude of its own and no AI host offering one is connected. Ask the person whose AI host it is to start it (the heart), or hook up an AI of your own in Settings." })) };
    let Some(prov_env) = Work::claude_env(&prov) else { return one_line(json!({ "type": "bridge_error", "text": "Your chosen AI provider is not Claude, so it cannot run Work. Pick a Claude model in the dropdown." })) };
    let kind = s(&prov, "kind");
    if (kind == "host" || kind == "claude-cli") && hub.root.join("ai-host.off").exists() { return one_line(json!({ "type": "bridge_error", "text": AI_OFF_TEXT })); }
    let owner = s(&prov, "owner"); let nick = s(&prov, "nickname");
    let host_id = s(&prov, "hostId"); let prov_id = s(&prov, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default();
    if kind == "host-claude" && !ai.online(&host_id) { return one_line(json!({ "type": "bridge_error", "text": format!("{nick}{} is offline right now. Pick another AI in the dropdown, or wait for it.", if owner.is_empty() { String::new() } else { format!(" ({}'s AI host)", hub.mods.name_of(&owner)) }) })); }
    if kind == "host-claude" && !s(&prov, "capped").is_empty() { return one_line(json!({ "type": "bridge_error", "text": format!("{nick}: {}. Pick another AI or wait for the window to reset.", s(&prov, "capped")) })); }
    let source = if kind == "host" { "hub".to_string() } else if kind == "host-claude" { s(&prov, "id") } else { String::new() };
    // a Claude session lives where it ran (relayed runs run here, on this hub)
    let wher = if kind == "host-claude" && s(&prov, "remoteKind") == "claude-cli-work" { host_id.clone() } else { "hub".to_string() };
    let resume = !fresh && !s(&st, "claudeSessionId").is_empty() && { let w0 = s(&st, "claudeSessionWhere"); (if w0.is_empty() { "hub".to_string() } else { w0 }) == wher };
    if resume { args.push("--resume".into()); args.push(s(&st, "claudeSessionId")); }
    { let wh = wher.clone(); w.update(a, |m| { m.insert("claudeSessionWhere".into(), json!(wh)); }); }
    // attached files (the marker block at the end of the message): Claude gets their paths and may read that folder
    let wm = hub.mods.files.work_message(a, &message, json!({ "project": wd.project }));
    for d in wm["dirs"].as_array().cloned().unwrap_or_default() { if let Some(d) = d.as_str() { args.push("--add-dir".into()); args.push(d.into()); } }
    let text = wm["text"].as_str().unwrap_or(&message).to_string();
    let input = if context.is_empty() { text } else { format!("{text}\n\n---\n{context}") };
    if !source.is_empty() { ai.usage.note_activity(&source); }
    let (tx, rx) = mpsc::unbounded_channel();
    let run_id = vr_core::crypto::random_hex(8);
    w.runs.lock().unwrap().insert(a.username.clone(), Run { id: run_id.clone(), tx: tx.clone(), pid: None, remote: String::new(), pending: HashMap::new() });
    w.busy_changed.notify_one();
    // the page hears something every 15 s while Claude thinks
    { let (h, u, id) = (hub.clone(), a.username.clone(), run_id.clone()); tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(15)).await; let alive = h.mods.work.runs.lock().unwrap().get(&u).map(|r| r.id == id).unwrap_or(false); if !alive { return; } h.mods.work.emit(&u, json!({ "type": "ping" })); } }); }
    let mut relay_env: Vec<(String, String)> = vec![];
    if kind == "host-claude" && s(&prov, "remoteKind") != "claude-cli-work" {
        // a host's Claude login, shared as AI only: Claude Code runs HERE, only its model calls travel to the host
        let tok = w.llm_token(&a.username, &host_id, &prov_id);
        relay_env = vec![("ANTHROPIC_BASE_URL".into(), format!("http://127.0.0.1:{}/llm/{tok}", hub.cfg.port)), ("ANTHROPIC_AUTH_TOKEN".into(), "viberoller-relay".into()), ("ANTHROPIC_API_KEY".into(), String::new()), ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into())];
        w.emit(&a.username, json!({ "type": "bridge_info", "text": format!("Using {nick}{}; the work happens here on the server.", if owner.is_empty() { String::new() } else { format!(" ({}'s AI)", hub.mods.name_of(&owner)) }) }));
    } else if kind == "host-claude" {
        // legacy: Claude Code runs on the host (only for a host that allows work)
        let payload = json!({ "account": a.username, "display": a.display, "project": wd.project, "hub": wd.hub, "branch": wd.branch, "repoUrl": w.repo_url_for(&wd), "message": input, "systemPrompt": w.system_prompt(a, &wd), "model": model,
            "resume": if resume { s(&st, "claudeSessionId") } else { String::new() }, "permissionMode": hub.cfg.get("permissionMode"), "allowedTools": hub.cfg.get("allowedTools"), "hubUrl": hub.mods.public_url(&hub.cfg), "env": Value::Object(w.work_env(&a.username).into_iter().map(|(k, v)| (k, json!(v))).collect()) });
        match ai.run_work(&host_id, &prov_id, payload) {
            Err(e) => { w.runs.lock().unwrap().remove(&a.username); w.busy_changed.notify_one(); let _ = tx.send(line(json!({ "type": "bridge_error", "text": e.msg }))); let _ = tx.send(line(json!({ "type": "bridge_done", "code": 1 }))); drop(tx); return Routed::Answer(ndjson(rx)); }
            Ok((job, _token, mut events)) => {
                if let Some(r) = w.runs.lock().unwrap().get_mut(&a.username) { r.remote = job; }
                w.emit(&a.username, json!({ "type": "bridge_info", "text": format!("Running on {nick}{}.", if owner.is_empty() { " (AI host)".to_string() } else { format!(" ({}'s AI host)", hub.mods.name_of(&owner)) }) }));
                let (h, a2, id) = (hub.clone(), a.clone(), run_id.clone());
                tokio::spawn(async move {
                    let w = &h.mods.work;
                    while let Some(m) = events.recv().await {
                        match m {
                            super::ai_hosts::WorkMsg::Event(ev) => w.on_event(&h, &a2, ev, &source),
                            super::ai_hosts::WorkMsg::Done(c) => { w.finish(&a2, &id, json!(c)); return; }
                            super::ai_hosts::WorkMsg::Error(t) => { w.emit(&a2.username, json!({ "type": "bridge_error", "text": t })); w.finish(&a2, &id, json!(1)); return; }
                        }
                    }
                    w.finish(&a2, &id, json!(1));
                });
                drop(tx);
                return Routed::Answer(ndjson(rx));
            }
        }
    }
    // ---- here: claudeCommand / claudeArgs only for the regression runner's fake Claude; else the real `claude`
    let command = hub.cfg.get("claudeCommand").and_then(Value::as_str).unwrap_or("claude").to_string();
    let pre: Vec<String> = hub.cfg.get("claudeArgs").and_then(Value::as_array).map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut cmd = tokio::process::Command::new(&command);
    cmd.args(&pre).args(&args).current_dir(&wd.dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(false);
    for (k, v) in prov_env.iter().chain(relay_env.iter()).chain(w.work_env(&a.username).iter()) { cmd.env(k, v); }
    #[cfg(windows)]
    { cmd.creation_flags(0x0800_0000); }
    let (h, a2, id) = (hub.clone(), a.clone(), run_id.clone());
    drop(tx);
    tokio::spawn(async move {
        let w = &h.mods.work;
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => { w.emit(&a2.username, json!({ "type": "bridge_error", "text": format!("Could not start claude: {e}") })); w.finish(&a2, &id, json!(-2)); return; }
        };
        if let Some(r) = w.runs.lock().unwrap().get_mut(&a2.username) { r.pid = child.id(); }
        if let Some(mut stdin) = child.stdin.take() { let inp = input.clone(); tokio::spawn(async move { let _ = stdin.write_all(inp.as_bytes()).await; let _ = stdin.shutdown().await; }); }
        let err_task = child.stderr.take().map(|e| { let (h, u) = (h.clone(), a2.username.clone()); tokio::spawn(async move { use tokio::io::AsyncReadExt; let mut e = e; let mut buf = vec![0u8; 8192]; while let Ok(n) = e.read(&mut buf).await { if n == 0 { break; } h.mods.work.emit(&u, json!({ "type": "stderr", "text": String::from_utf8_lossy(&buf[..n]) })); } }) });
        if let Some(out) = child.stdout.take() {
            let mut lines = tokio::io::BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let l = l.trim(); if l.is_empty() { continue; }
                match serde_json::from_str::<Value>(l) { Ok(ev) => w.on_event(&h, &a2, ev, &source), Err(_) => w.emit(&a2.username, json!({ "type": "raw", "text": l })) }
            }
        }
        let code = child.wait().await.ok().and_then(|s| s.code()).map(|c| json!(c)).unwrap_or(Value::Null);
        if let Some(t) = err_task { let _ = t.await; }
        w.finish(&a2, &id, code);
    });
    Routed::Answer(ndjson(rx))
}

/// The MCP tools of a run (here: /internal/<tool> with this hub's secret; on a host: /node/job/<id>/internal/<tool>).
pub async fn tool(hub: &Arc<Hub>, which: &str, account: &str, b: &Value) -> Value {
    let (name, args) = (s(b, "name"), if b["args"].is_object() { b["args"].clone() } else { json!({}) });
    let m = &hub.mods;
    if which == "board" {
        let Some(op) = super::board::tool_op(&name) else { return json!({ "ok": false, "error": format!("Unknown board tool: {name}") }) };
        if !m.has(account) { return json!({ "ok": false, "error": "Unknown account." }); }
        let out = m.board_op(op, &m.actor(account, true), &args);
        return if out.status == 200 { json!({ "ok": true, "result": out.body }) } else { json!({ "ok": false, "error": out.body["error"] }) };
    }
    let Some(a) = m.auth.account(account) else { return json!({ "ok": false, "error": "Unknown account." }) };
    let w = &m.work;
    if which == "team" {
        if name.starts_with("notes_") { return m.notes.tool(&name, &args, &m.name_of(account)); }
        match name.as_str() {
            "sessions_search" => {
                let q: String = args["q"].as_str().or(args["query"].as_str()).unwrap_or("").chars().take(200).collect();
                let me = m.name_of(account);
                let items: Vec<Value> = archive_search(&a, &hub.mods.mills_dir(), &q).into_iter().take(20).map(|it| json!({ "id": it["id"], "title": it["title"], "savedAt": it["savedAt"], "entries": it["count"], "hits": it["hits"],
                    "snippets": it["snippets"].as_array().map(|x| x.iter().map(|sn| json!(format!("{}: {}", if sn["cls"] == json!("me") { me.clone() } else { "Claude".into() }, s(sn, "text")))).collect::<Vec<_>>()).unwrap_or_default() })).collect();
                return json!({ "ok": true, "result": { "query": q, "items": items } });
            }
            "my_providers" => {
                let st = w.state(&a);
                let list: Vec<Value> = providers_list(hub, account).into_iter().map(|p| json!({ "id": p["id"], "label": p["label"], "kind": p["kind"], "owner": if s(&p, "owner").is_empty() { json!("host") } else { p["owner"].clone() }, "shared": p["shared"], "models": p["models"] })).collect();
                return json!({ "ok": true, "result": { "providers": list, "current": if s(&st, "provider").is_empty() { "host".into() } else { s(&st, "provider") }, "model": s(&st, "model") } });
            }
            _ => return super::chat::team_tool(hub, &name, &args, &a), // the chat, presence, calls, servers
        }
    }
    // pc-tool: the person's own AI host with direct computer access first (the native app), else the browser's open folder
    if let Some(h) = m.ai.pc_host_for(account) {
        match m.ai.pc_tool(&h, &name, &args).await { Ok(v) => return v, Err(e) => if !w.busy(account) { return json!({ "ok": false, "error": format!("{}'s computer did not answer: {}", a.display, e.msg) }) } }
    }
    let id = vr_core::crypto::random_hex(8);
    let (otx, orx) = oneshot::channel();
    { let mut runs = w.runs.lock().unwrap(); match runs.get_mut(account) { Some(r) => { r.pending.insert(id.clone(), otx); } None => return json!({ "ok": false, "error": format!("{}'s browser is not connected (and no AI host of theirs offers their computer).", a.display) }) } }
    w.emit(account, json!({ "type": "pc_tool", "id": id, "name": name, "args": args }));
    match tokio::time::timeout(Duration::from_secs(120), orx).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => json!({ "ok": false, "error": "Run ended" }),
        Err(_) => { if let Some(r) = w.runs.lock().unwrap().get_mut(account) { r.pending.remove(&id); } json!({ "ok": false, "error": "Timed out waiting for the browser." }) }
    }
}
/// Everything a person may pick (bridge/providers.js list): this hub's Claude, the own / shared ones, the hosts'.
pub fn providers_list(hub: &Hub, user: &str) -> Vec<Value> {
    let w = &hub.mods.work;
    let mut out = vec![]; if w.hub_claude() { out.push(w.host_entry()); }
    for p in w.own_providers() { if p["shared"] == json!(true) || p["owner"] == json!(user) {
        let mut v = json!({ "id": p["id"], "kind": p["kind"], "label": p["label"], "owner": p["owner"], "shared": p["shared"] == json!(true), "models": if p["models"].is_array() { p["models"].clone() } else { json!([]) }, "hasKey": !s(&p, "apiKey").is_empty(), "createdAt": p["createdAt"] });
        for k in ["baseUrl", "configDir"] { if !s(&p, k).is_empty() { v[k] = p[k].clone(); } }
        out.push(v);
    } }
    out.extend(hub.mods.ai.providers_for(user));
    out
}

// ---------- past sessions (Archive/[<name>/]<id>.json) ----------
fn archive_dir(a: &Account, mills: &Path, root: &Path) -> PathBuf { if a.dir == root { mills.join("Archive") } else { mills.join("Archive").join(&a.username) } }
fn archive_entries(dir: &Path) -> Vec<Value> { std::fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".json")).filter_map(|e| read_json(&e.path())).collect()).unwrap_or_default() }
fn archive_list(dir: &Path) -> Vec<Value> {
    let mut v: Vec<Value> = archive_entries(dir).into_iter().map(|e| json!({ "id": e["id"], "title": e["title"], "savedAt": e["savedAt"], "count": e["transcript"].as_array().map(|t| t.len()).unwrap_or(0) })).collect();
    v.sort_by(|a, b| s(b, "savedAt").cmp(&s(a, "savedAt"))); v
}
pub fn archive_search(a: &Account, mills: &Path, q: &str) -> Vec<Value> {
    let root = mills.join("bridge");
    let dir = archive_dir(a, mills, &root);
    let words: Vec<String> = q.to_lowercase().split_whitespace().take(8).map(String::from).collect();
    if words.is_empty() { return archive_list(&dir); }
    let mut out = vec![];
    for e in archive_entries(&dir) {
        let lines: Vec<Value> = e["transcript"].as_array().cloned().unwrap_or_default().into_iter().filter(|t| t["text"].is_string()).collect();
        let hay = (s(&e, "title") + "\n" + &lines.iter().map(|t| s(t, "text")).collect::<Vec<_>>().join("\n")).to_lowercase();
        if !words.iter().all(|w| hay.contains(w.as_str())) { continue; }
        let (mut snippets, mut hits) = (vec![], 0usize);
        for t in &lines {
            let text = s(t, "text"); let low = text.to_lowercase();
            for w in &words { hits += low.matches(w.as_str()).count(); }
            // the first match, as a character position (the text may hold any letters)
            let first = words.iter().filter_map(|w| low.find(w.as_str())).min();
            if let (Some(f), true) = (first, snippets.len() < 3) {
                let chars: Vec<char> = text.chars().collect(); let fc = low[..f].chars().count().min(chars.len());
                let start = fc.saturating_sub(60); let end = (fc + 90).min(chars.len());
                let body: String = chars[start..end].iter().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
                snippets.push(json!({ "cls": t["cls"], "text": format!("{}{}{}", if start > 0 { "…" } else { "" }, body, if fc + 90 < chars.len() { "…" } else { "" }) }));
            }
        }
        out.push(json!({ "id": e["id"], "title": e["title"], "savedAt": e["savedAt"], "count": lines.len(), "hits": hits, "snippets": snippets }));
    }
    out.sort_by(|a, b| b["hits"].as_u64().cmp(&a["hits"].as_u64()).then(s(b, "savedAt").cmp(&s(a, "savedAt"))));
    out
}
fn clean_title(t: &str) -> String { let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect(); if t.is_empty() { "Untitled conversation".into() } else { t } }
fn archive_file(dir: &Path, id: &str) -> Option<PathBuf> { if id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { Some(dir.join(format!("{id}.json"))) } else { None } }

// ---------- shared Work sessions (watch-only) ----------
impl Work {
    fn share_line(&self, owner: &str, t: &str, text: &str) {
        if text.is_empty() { return; }
        let mut sh = self.shares.lock().unwrap(); let Some(s) = sh.get_mut(owner) else { return };
        s.n += 1; s.log.push(json!({ "n": s.n, "t": t, "text": text.chars().take(20000).collect::<String>(), "ts": iso() }));
        if s.log.len() > 400 { let k = s.log.len() - 400; s.log.drain(0..k); }
        s.woke.notify_waiters();
    }
    fn share_event(&self, owner: &str, ev: &Value) {
        if !self.shares.lock().unwrap().contains_key(owner) { return; }
        if ev["type"] == json!("assistant") && ev["message"].is_object() {
            for b in ev["message"]["content"].as_array().cloned().unwrap_or_default() {
                if b["type"] == json!("text") && !s(&b, "text").trim().is_empty() { self.share_line(owner, "claude", &s(&b, "text")); }
                else if b["type"] == json!("tool_use") {
                    let i = &b["input"];
                    let target = ["path", "file_path", "command", "pattern", "url", "query"].iter().find(|k| !i[**k].is_null()).map(|k| match &i[*k] { Value::String(x) => x.clone(), v => v.to_string() }).unwrap_or_default();
                    let name = s(&b, "name"); let name = name.strip_prefix("mcp__pc__").map(|n| format!("their PC › {n}")).unwrap_or(name);
                    self.share_line(owner, "tool", &format!("{name}{}", if target.is_empty() { String::new() } else { format!("  {}", target.chars().take(160).collect::<String>()) }));
                }
            }
        } else if ev["type"] == json!("result") { self.share_line(owner, "sys", if ev["is_error"] == json!(true) { "Claude stopped with an error" } else { "Done" }); }
    }
    pub fn shares_for(&self, user: &str) -> Vec<Value> { self.shares.lock().unwrap().iter().filter(|(o, _)| *o != user).map(|(o, s)| json!({ "user": o, "conv": s.conv, "since": s.since })).collect() }
    pub fn shares_all(&self) -> Vec<Value> { self.shares.lock().unwrap().iter().map(|(o, s)| json!({ "user": o, "conv": s.conv, "since": s.since })).collect() }
    pub fn share_of(&self, owner: &str) -> String { self.shares.lock().unwrap().get(owner).map(|s| s.conv.clone()).unwrap_or_default() }
    pub fn share_stop(&self, owner: &str) { let gone = self.shares.lock().unwrap().remove(owner); if let Some(s) = gone { s.woke.notify_waiters(); self.shares_changed.notify_one(); } }
}

/// The Work routes (signed in as `a`); None = not one of them.
pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    let w = &hub.mods.work; let u = &a.username;
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let p = r.path.as_str();
    Some(match p {
        "/api/chat" if post => chat(hub, a, r).await,
        "/api/stop" if post => { w.stop(hub, u); answer(200, json!({ "ok": true })) }
        "/api/pc-result" if post => {
            let b = r.json();
            let entry = w.runs.lock().unwrap().get_mut(u).and_then(|run| run.pending.remove(&s(&b, "id")));
            if let Some(tx) = entry { let mut v = json!({ "ok": b["ok"].as_bool().unwrap_or(false) || (!b["ok"].is_null() && b["ok"] != json!(false) && b["ok"] != json!(0) && b["ok"] != json!("")) }); for k in ["result", "error"] { if let Some(x) = b.get(k) { v[k] = x.clone(); } } let _ = tx.send(v); }
            answer(200, json!({ "ok": true }))
        }
        "/api/usage" if get => answer(200, w.usage_status(hub, a)),
        "/api/model" if post => {
            let b = r.json(); let st = w.state(a); let ai = &hub.mods.ai;
            let want = if b.get("provider").is_none() { s(&st, "provider") } else { s(&b, "provider") };
            let prov = if !want.is_empty() && want != "host" { w.provider_get(ai, &want, u) } else { w.default_for(ai, u).or_else(|| w.provider_get(ai, "host", "")) };
            let Some(prov) = prov else { return Some(answer(400, json!({ "error": "Unknown provider." }))) };
            if Work::claude_env(&prov).is_none() { return Some(answer(400, json!({ "error": "That provider is not Claude; Work runs need Claude Code. Use it for the API or the Unity section instead." }))); }
            let ids: Vec<String> = if prov["kind"] == json!("host") { w.model_ids() } else { prov["models"].as_array().map(|m| m.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
            let Some(model) = b["model"].as_str().map(String::from) else { return Some(answer(400, json!({ "error": "Unknown model." }))) };
            if !model.is_empty() && !ids.contains(&model) { return Some(answer(400, json!({ "error": "Unknown model." }))); }
            let pid = s(&prov, "id");
            let st = w.update(a, |m| {
                if model.is_empty() { m.remove("model"); } else { m.insert("model".into(), json!(model)); }
                if want.is_empty() || want == "host" { m.remove("provider"); } else { m.insert("provider".into(), json!(pid)); }
                if m.contains_key("provider") && model.is_empty() { if let Some(f) = ids.first() { m.insert("model".into(), json!(f)); } }
            });
            let eff = w.effective(ai, u, &s(&st, "provider")).map(|p| s(&p, "id")).unwrap_or_default();
            answer(200, json!({ "model": s(&st, "model"), "provider": if s(&st, "provider").is_empty() { "host".into() } else { s(&st, "provider") }, "effective": w.effective_model(a), "effectiveProvider": eff }))
        }
        "/api/projects" if get => answer(200, json!({ "projects": w.project_list(), "project": w.project_of(a) })),
        "/api/workspace" if post => {
            let b = r.json();
            let want = if !s(&b, "project").is_empty() { s(&b, "project") } else if !b["workspace"].is_null() && b["workspace"] != json!("") { HUB_PROJECT.into() } else { String::new() };
            let pj = match w.wt.project(&want) { Ok(p) => p, Err(e) => return Some(answer(400, json!({ "error": e }))) };
            if w.busy(u) { return Some(answer(409, json!({ "error": "Wait for the current reply (or stop it) before switching." }))); }
            let wd = match w.wt.ensure(u, &pj.name) { Ok(x) => x, Err(e) => return Some(answer(500, json!({ "error": format!("Could not prepare your worktree: {e}") }))) };
            let switched = w.project_of(a) != pj.name;
            w.update(a, |m| { if switched { m.remove("claudeSessionId"); } if pj.hub { m.remove("project"); } else { m.insert("project".into(), json!(pj.name)); } });
            w.check_fresh(a);
            answer(200, json!({ "project": pj.name, "dir": wd.dir.display().to_string(), "branch": wd.branch }))
        }
        // ----- the Work conversation + queue, kept across restarts -----
        "/api/work/current" if get => answer(200, work_current(a)),
        "/api/work/current" if post => {
            let b = r.json(); let mut st = work_current(a);
            let Some(list) = b["transcript"].as_array() else { return Some(answer(400, json!({ "error": "transcript must be a list." }))) };
            let mut out: Vec<Value> = list.iter().skip(list.len().saturating_sub(3000)).filter(|t| ["me", "claude", "tool"].contains(&t["cls"].as_str().unwrap_or("")) && t["text"].is_string()).map(|t| json!({ "cls": t["cls"], "text": s(t, "text").chars().take(200 * 1024).collect::<String>() })).collect();
            let mut total = 0usize; let mut keep = 0usize;
            for (i, t) in out.iter().enumerate().rev() { total += t["text"].as_str().map(|x| x.encode_utf16().count()).unwrap_or(0); if total > 6 * 1024 * 1024 { keep = i + 1; break; } }
            if keep > 0 { out.drain(0..keep); }
            let n = out.len();
            st["transcript"] = json!(out);
            st["archiveId"] = b["archiveId"].as_str().filter(|x| !x.is_empty()).map(|x| json!(x.chars().take(80).collect::<String>())).unwrap_or(Value::Null);
            st["archiveTitle"] = b["archiveTitle"].as_str().filter(|x| !x.is_empty()).map(|x| json!(x.chars().take(200).collect::<String>())).unwrap_or(Value::Null);
            save_work_current(a, &mut st);
            answer(200, json!({ "ok": true, "items": n }))
        }
        "/api/work/queue" if post => {
            let b = r.json(); let mut st = work_current(a);
            let Some(q) = b["queue"].as_array() else { return Some(answer(400, json!({ "error": "queue must be a list." }))) };
            let clean = |m: &Value| m.as_str().filter(|x| !x.trim().is_empty()).map(|x| json!(x.chars().take(50 * 1024).collect::<String>()));
            st["queue"] = json!(q.iter().filter_map(clean).take(50).collect::<Vec<_>>());
            st["sendNow"] = clean(&b["sendNow"]).unwrap_or(Value::Null);
            let n = st["queue"].as_array().map(|x| x.len()).unwrap_or(0);
            save_work_current(a, &mut st);
            answer(200, json!({ "ok": true, "queue": n }))
        }
        // ----- shared Work sessions -----
        "/api/work/share" if post => {
            let b = r.json(); let conv = s(&b, "conv");
            if conv.is_empty() { w.share_stop(u); return Some(answer(200, json!({ "share": "" }))); }
            if conv != "room" { return Some(answer(400, json!({ "error": "A Work session can only be shared with everyone." }))); }
            let since = iso();
            let kinds = |c: &str| match c { "me" => Some("user"), "claude" => Some("claude"), "tool" => Some("tool"), "sys" => Some("sys"), _ => None };
            let mut log = vec![]; let mut n = 0;
            let tr = b["transcript"].as_array().cloned().unwrap_or_default();
            for t in tr.iter().skip(tr.len().saturating_sub(150)) { if let (Some(k), Some(text)) = (kinds(t["cls"].as_str().unwrap_or("")), t["text"].as_str()) { n += 1; log.push(json!({ "n": n, "t": k, "text": text.chars().take(20000).collect::<String>(), "ts": since })); } }
            { let mut sh = w.shares.lock().unwrap(); let prev = sh.remove(u); let live = prev.as_ref().map(|p| p.live.clone()).unwrap_or(json!({ "typing": "", "queue": [] }));
              if let Some(p) = &prev { p.woke.notify_waiters(); }
              sh.insert(u.clone(), Share { conv: conv.clone(), since, n, log, live, ver: 0, woke: prev.map(|p| p.woke).unwrap_or_default() }); }
            w.shares_changed.notify_one();
            answer(200, json!({ "share": conv }))
        }
        "/api/work/live" if post => {
            let b = r.json(); let mut sh = w.shares.lock().unwrap();
            let Some(sv) = sh.get_mut(u) else { return Some(answer(200, json!({ "ok": false }))) };
            sv.live = json!({ "typing": b["typing"].as_str().map(|t| t.chars().take(4000).collect::<String>()).unwrap_or_default(), "queue": b["queue"].as_array().map(|q| q.iter().take(20).map(|x| json!(match x { Value::String(t) => t.chars().take(2000).collect::<String>(), v => v.to_string() })).collect::<Vec<_>>()).unwrap_or_default() });
            sv.ver += 1; sv.woke.notify_waiters();
            answer(200, json!({ "ok": true }))
        }
        "/api/work/watch" if get => {
            let owner = r.param("user").unwrap_or_default(); let after: u64 = r.param("after").and_then(|x| x.parse().ok()).unwrap_or(0);
            let ver: Option<u64> = r.param("ver").and_then(|x| x.parse().ok()); let since = r.param("since").unwrap_or_default();
            let view = || -> (Value, bool) {
                let sh = w.shares.lock().unwrap();
                let Some(sv) = sh.get(&owner).filter(|_| owner != *u) else { return (json!({ "stopped": true }), true) };
                let restart = since != sv.since;
                let ready = restart || sv.log.iter().any(|e| e["n"].as_u64().unwrap_or(0) > after) || ver != Some(sv.ver);
                (json!({ "since": sv.since, "conv": sv.conv, "busy": w.busy(&owner), "live": sv.live, "ver": sv.ver, "events": sv.log.iter().filter(|e| restart || e["n"].as_u64().unwrap_or(0) > after).cloned().collect::<Vec<_>>(), "after": sv.n, "restart": restart }), ready)
            };
            let woke = w.shares.lock().unwrap().get(&owner).map(|s| s.woke.clone());
            if let Some(woke) = woke {
                let n = woke.notified(); tokio::pin!(n); n.as_mut().enable();
                let (v, ready) = view(); if ready { return Some(answer(200, v)); }
                let _ = tokio::time::timeout(Duration::from_secs(25), n).await;
            }
            answer(200, view().0)
        }
        // ----- past sessions -----
        p if post && p.starts_with("/api/archive/") => {
            let dir = archive_dir(a, &hub.mods.mills_dir(), &hub.root); let b = r.json();
            match p {
                "/api/archive/list" => answer(200, json!({ "items": archive_list(&dir) })),
                "/api/archive/search" => answer(200, json!({ "items": archive_search(a, &hub.mods.mills_dir(), &s(&b, "q").chars().take(200).collect::<String>()) })),
                "/api/archive/save" => {
                    let Some(tr) = b["transcript"].as_array().filter(|t| !t.is_empty()) else { return Some(answer(400, json!({ "error": "Nothing to save yet." }))) };
                    let sid = s(&w.state(a), "claudeSessionId");
                    if sid.is_empty() { return Some(answer(400, json!({ "error": "This conversation has no Claude session yet." }))); }
                    let id = if s(&b, "id").is_empty() { vr_core::crypto::random_hex(8) } else { s(&b, "id") };
                    let Some(f) = archive_file(&dir, &id) else { return Some(answer(400, json!({ "error": "Bad conversation id." }))) };
                    let prev = read_json(&f).unwrap_or(json!({}));
                    let _ = std::fs::create_dir_all(&dir);
                    let title = clean_title(if s(&b, "title").is_empty() { prev["title"].as_str().unwrap_or("") } else { b["title"].as_str().unwrap_or("") });
                    let entry = json!({ "id": id, "title": title, "savedAt": iso(), "sessionId": sid, "transcript": tr.iter().filter(|t| t["cls"].is_string() && t["text"].is_string()).cloned().collect::<Vec<_>>() });
                    write_json(&f, &entry);
                    answer(200, json!({ "id": id, "title": title }))
                }
                "/api/archive/open" => {
                    if w.busy(u) { return Some(answer(409, json!({ "error": "Claude is working. Stop it first." }))); }
                    let Some(f) = archive_file(&dir, &s(&b, "id")) else { return Some(answer(400, json!({ "error": "Bad conversation id." }))) };
                    let Some(entry) = read_json(&f) else { return Some(answer(404, json!({ "error": "Conversation not found." }))) };
                    let sid = entry["sessionId"].clone(); w.update(a, |m| { m.insert("claudeSessionId".into(), sid); });
                    answer(200, entry)
                }
                "/api/archive/rename" => {
                    let Some(f) = archive_file(&dir, &s(&b, "id")) else { return Some(answer(400, json!({ "error": "Bad conversation id." }))) };
                    let Some(mut entry) = read_json(&f) else { return Some(answer(404, json!({ "error": "Conversation not found." }))) };
                    entry["title"] = json!(clean_title(&s(&b, "title"))); write_json(&f, &entry);
                    answer(200, json!({ "id": b["id"], "title": entry["title"] }))
                }
                "/api/archive/delete" => { let Some(f) = archive_file(&dir, &s(&b, "id")) else { return Some(answer(400, json!({ "error": "Bad conversation id." }))) }; let _ = std::fs::remove_file(f); answer(200, json!({ "ok": true })) }
                _ => return None,
            }
        }
        _ => return None,
    })
}
fn work_current_file(a: &Account) -> PathBuf { a.dir.join("work-current.json") }
fn work_current(a: &Account) -> Value {
    let s0 = read_json(&work_current_file(a)).filter(Value::is_object).unwrap_or(json!({}));
    let or_null = |k: &str| if s0[k].is_null() || s0[k] == json!("") { Value::Null } else { s0[k].clone() };
    json!({ "transcript": if s0["transcript"].is_array() { s0["transcript"].clone() } else { json!([]) }, "archiveId": or_null("archiveId"), "archiveTitle": or_null("archiveTitle"), "queue": if s0["queue"].is_array() { s0["queue"].clone() } else { json!([]) }, "sendNow": or_null("sendNow"), "updatedAt": or_null("updatedAt") })
}
fn save_work_current(a: &Account, st: &mut Value) {
    st["updatedAt"] = json!(iso());
    let f = work_current_file(a); let tmp = f.with_extension("json.tmp");
    if std::fs::write(&tmp, st.to_string()).is_ok() { let _ = std::fs::rename(&tmp, &f); }
}

/// /llm/<token>/…: this hub's own Claude Code reaching a host's shared login (local only, per-run token).
pub async fn llm(hub: &Arc<Hub>, r: &Req) -> Routed {
    let rest = &r.path["/llm/".len()..];
    let (tok, sub) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let behind = hub.cfg.get("behindProxy") == Some(&json!(true));
    let outside = r.header("cf-connecting-ip").is_some() || (behind && (r.header("x-forwarded-for").is_some() || r.header("x-real-ip").is_some()));
    let ent = if tok.len() == 48 && tok.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) && !sub.is_empty() { hub.mods.work.llm.lock().unwrap().get(tok).cloned() } else { None };
    let Some((_, host, prov, exp)) = ent.filter(|e| e.3 > now_ms() && r.peer.ip().is_loopback() && !outside) else { return answer(401, json!({ "type": "error", "error": { "type": "authentication_error", "message": "Unknown relay." } })) };
    let _ = exp;
    let path = if r.query.is_empty() { sub.to_string() } else { format!("{sub}?{}", r.query) };
    let headers: Vec<(String, String)> = r.headers.iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
    match hub.mods.ai.login_fetch(&host, &prov, &path, r.method.as_str(), headers, r.body.to_vec()).await {
        Ok(rel) => {
            if rel.headers.iter().any(|(k, _)| k.starts_with("anthropic-ratelimit-unified-")) { super::push_ai_state(hub).await; }
            let mut b = hyper::Response::builder().status(rel.status);
            for (k, v) in &rel.headers { b = b.header(k.as_str(), v.as_str()); }
            b = b.header("cache-control", "no-store").header("x-accel-buffering", "no");
            Routed::Answer(b.body(super::stream_body(rel.body)).expect("response"))
        }
        Err(e) => answer(if e.status == 0 { 503 } else { e.status }, json!({ "type": "error", "error": { "type": if e.status == 429 { "rate_limit_error" } else { "overloaded_error" }, "message": e.msg } })),
    }
}
/// The background freshness checks: every 5 minutes for anyone who ran something in the last day.
pub fn start(hub: &Arc<Hub>) {
    let h = hub.clone();
    tokio::spawn(async move { loop {
        tokio::time::sleep(Duration::from_secs(300)).await;
        for a in h.mods.auth.accounts() { let last = h.mods.work.state(&a)["lastRunAt"].as_u64().unwrap_or(0); if last > 0 && now_ms() - last < 86_400_000 { h.mods.work.check_fresh(&a); } }
    } });
    // the shared sessions to the JavaScript hub (its room poll lists them; pages follow through it)
    let h = hub.clone();
    tokio::spawn(async move { loop {
        h.mods.work.shares_changed.notified().await;
        let list = h.mods.work.shares_all();
        h.mods.chat.room_changed(); // the room's long poll lists them
        crate::front::internal(&h, "/internal/front/shares", json!({ "shares": list }).to_string()).await;
    } });
    let h = hub.clone();
    tokio::spawn(async move { loop {
        h.mods.work.busy_changed.notified().await;
        let busy = h.mods.work.any_busy() || h.mods.api.busy() || h.mods.unity.busy() || super::unity_routes::room_busy(&h);
        crate::front::internal(&h, "/internal/front/work-busy", json!({ "busy": busy }).to_string()).await;
    } });
}
pub fn same_secret(a: &str, b: &str) -> bool { use subtle::ConstantTimeEq; bool::from(vr_core::crypto::sha256(a.as_bytes()).ct_eq(&vr_core::crypto::sha256(b.as_bytes()))) }
