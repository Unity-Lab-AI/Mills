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
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use pgm_context_internal::assembly::{lint, Class, LintHit, Segment};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::hub::{answer, AiHub, Resp};
use crate::questions;
use crate::sessions::{self, Sx};
use pgm_collab_internal::worktree::{Worktrees, Wt, HUB_PROJECT, NO_PROJECT};
use pgm_core_internal::config::Config;
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

const AI_OFF_TEXT: &str = "The host's AI is switched off right now (its host paused it). Chat, the board and your own AI keep working; pick your own AI in the model dropdown, or try again once it is back.";
/// The kind a host's `jcode` provider has on the hub: Work runs ON that host through `jcodepgm run --ndjson` (WORKPLAN 9c).
const HOST_JCODE: &str = "host-jcode";
const STATE_KEYS: [&str; 7] = ["provider", "model", "claudeSessionId", "claudeSessionWhere", "ctx", "project", "lastRunAt"];

/// A run of one session (its key: the person for the primary session, "<user>#<id>" for the others). `last` = when it
/// last sent something of its own (not the hub's pings): the liveness check ends a run silent for too long.
struct Run { id: String, tx: mpsc::UnboundedSender<Result<Bytes, String>>, pid: Option<u32>, remote: String, pending: HashMap<String, oneshot::Sender<Value>>, ev: RunLog, last: u64, answered: bool, prov: String, project: String, branch: String, started: u64 }
/// Everything a run has sent, so a page that reloads (or a second tab) can join it again: GET /api/work/attach.
/// `base` = how many of the oldest events were dropped (a run keeps its last RUN_LOG_MAX).
#[derive(Default, Clone)]
struct RunLog { base: usize, events: Vec<Value>, woke: Arc<tokio::sync::Notify> }
const RUN_LOG_MAX: usize = 20000;
impl RunLog {
    fn push(&mut self, ev: Value) { self.events.push(ev); if self.events.len() > RUN_LOG_MAX { let k = self.events.len() - RUN_LOG_MAX; self.events.drain(0..k); self.base += k; } self.woke.notify_waiters(); }
    fn next(&self) -> usize { self.base + self.events.len() }
    fn since(&self, after: usize) -> Vec<Value> { self.events.iter().skip(after.saturating_sub(self.base)).cloned().collect() }
}
/// A shared Work session: `sid` = which of the owner's sessions it shows (plans/multi-session.md § 7).
struct Share { conv: String, since: String, n: u64, log: Vec<Value>, live: Value, ver: u64, woke: Arc<Notify>, sid: String, key: String }
/// A question the AI asked (T-245): waits in `Work::asks` for its person, the timeout, or the end of the run.
struct Ask { user: String, key: String, sid: String, title: String, qs: Vec<Value>, at: u64, timeout_ms: u64, deadline: u64, result: Option<Value>, notify: Arc<Notify> }
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
    /// a run that just ended, for a few minutes: (its id, what it sent, when it ended)
    ended: Mutex<HashMap<String, (String, RunLog, u64)>>,
    llm: Mutex<HashMap<String, (String, String, String, u64)>>,
    fresh: Mutex<HashMap<String, Fresh>>,
    shares: Mutex<HashMap<String, Share>>,
    /// the questions the AI of a run asked and nobody answered yet (by id)
    asks: Mutex<HashMap<String, Ask>>,
    projects: Mutex<(u64, Value)>,
    /// the shared sessions changed (the JavaScript hub's room poll lists them)
    pub shares_changed: Notify,
    /// a run started or ended (the JavaScript hub restarts only while nobody's Claude is busy, here included)
    pub busy_changed: Notify,
}
use pgm_core_internal::json::{read_json, s, write_json};
/// The freshness note's key: the person's worktree of a project, or one of their sessions'.
fn fresh_key(u: &str, pj: &str, hex: Option<&str>) -> String { match hex { Some(h) => format!("{u}|{pj}|{h}"), None => format!("{u}|{pj}") } }
/// The session a worktree belongs to (its branch work/<user>.<hex>), None for the person's own.
fn session_of(u: &str, wd: &Wt) -> Option<String> { wd.branch.strip_prefix(&format!("work/{u}.")).map(String::from) }
fn line(v: Value) -> Result<Bytes, String> { Ok(Bytes::from(v.to_string() + "\n")) }
fn iso() -> String { pgm_core_internal::time::iso_now() }

impl Work {
    pub fn new(root: &Path, cfg: &Config) -> Work {
        Work { root: root.to_path_buf(), mills: root.join(".."), cfg: cfg.clone(), secret: pgm_core_internal::crypto::random_hex(24), wt: Worktrees::new(root, cfg), states: Mutex::default(), runs: Mutex::default(), ended: Mutex::default(), llm: Mutex::default(), fresh: Mutex::default(), shares: Mutex::default(), asks: Mutex::default(), projects: Mutex::new((0, json!([]))), shares_changed: Notify::new(), busy_changed: Notify::new() }
    }
    // ---------- each person's Work state ----------
    fn state_file(a: &Account) -> PathBuf { a.dir.join("work.json") }
    /// Their Work state (work.json; the first time, what their state.json held).
    pub fn state(&self, a: &Account) -> Value {
        if let Some(v) = self.states.lock().unwrap_or_else(|e| e.into_inner()).get(&a.username) { return v.clone(); }
        let v = read_json(&Self::state_file(a)).filter(Value::is_object).unwrap_or_else(|| {
            let old = read_json(&a.dir.join("state.json")).unwrap_or(json!({}));
            let mut m = Map::new(); for k in STATE_KEYS { if !old[k].is_null() { m.insert(k.into(), old[k].clone()); } }
            let v = Value::Object(m); write_json(&Self::state_file(a), &v); v
        });
        self.states.lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone(), v.clone());
        v
    }
    pub(crate) fn update_state(&self, a: &Account, f: impl FnOnce(&mut Map<String, Value>)) -> Value { self.update(a, f) }
    fn update(&self, a: &Account, f: impl FnOnce(&mut Map<String, Value>)) -> Value {
        let mut v = self.state(a); if !v.is_object() { v = json!({}); }
        f(v.as_object_mut().unwrap());
        write_json(&Self::state_file(a), &v);
        self.states.lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone(), v.clone());
        v
    }
    /// One account's key for its runs' tools: a run can act only as its own person, whatever account it names.
    pub fn secret_for(&self, account: &str) -> String { pgm_core_internal::crypto::hmac_hex(self.secret.as_bytes(), account.as_bytes()) }
    pub fn forget(&self, u: &str) { self.states.lock().unwrap_or_else(|e| e.into_inner()).remove(u); }
    pub fn drop_provider(&self, a: &Account, id: &str) { if self.state(a)["provider"] == json!(id) { self.update(a, |m| { m.remove("provider"); }); } }
    /// Is any session of theirs running (the primary one's run is keyed by the person, the others' by "<user>#<id>")?
    pub fn busy(&self, u: &str) -> bool { let p = format!("{u}#"); self.runs.lock().unwrap_or_else(|e| e.into_inner()).keys().any(|k| k == u || k.starts_with(&p)) }
    /// How many of their sessions run now (presence shows it, T-288).
    pub fn working_count(&self, u: &str) -> u64 { let p = format!("{u}#"); self.runs.lock().unwrap_or_else(|e| e.into_inner()).keys().filter(|k| *k == u || k.starts_with(&p)).count() as u64 }
    /// Where their most recently started running session works: (project, worktree branch, started at ms). Presence shows it (T-260).
    pub fn working_where(&self, u: &str) -> Option<(String, String, u64)> { let p = format!("{u}#"); self.runs.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(k, _)| *k == u || k.starts_with(&p)).map(|(_, r)| r).max_by_key(|r| r.started).map(|r| (r.project.clone(), r.branch.clone(), r.started)) }
    /// Is this one session running (its run key)?
    pub fn busy_key(&self, key: &str) -> bool { self.runs.lock().unwrap_or_else(|e| e.into_inner()).contains_key(key) }
    /// The run key of a run of theirs that waits for something from their browser: the primary session's first.
    fn run_key_of(&self, u: &str) -> Option<String> { let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner()); if runs.contains_key(u) { return Some(u.to_string()); } let p = format!("{u}#"); runs.keys().find(|k| k.starts_with(&p)).cloned() }
    /// The session (not the first one) whose run is this host job, for the tools its MCP relay calls ("" = none / the first).
    pub fn session_of_job(&self, job: &str) -> String { if job.is_empty() { return String::new(); } self.runs.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(_, r)| r.remote == job).and_then(|(k, _)| k.split_once('#').map(|x| x.1.to_string())).unwrap_or_default() }
    pub fn any_busy(&self) -> bool { !self.runs.lock().unwrap_or_else(|e| e.into_inner()).is_empty() }
    pub fn default_model_of(&self, a: &Account) -> String { self.default_model(a) }
    fn default_model(&self, a: &Account) -> String {
        let c = if a.dir == self.root { Value::Object(self.cfg.raw.clone()) } else { read_json(&a.dir.join("config.json")).unwrap_or(Value::Null) };
        s(&c, "model")
    }
    pub fn effective_model(&self, a: &Account) -> String { let m = s(&self.state(a), "model"); if m.is_empty() { self.default_model(a) } else { m } }
    /// A session's project (the first session's is project_of).
    pub fn project_in(&self, a: &Account, sx: &Sx) -> String { if self.is_none(a, sx) { return NO_PROJECT.into(); } let p = s(&sessions::sstate(self, a, sx), "project"); self.wt.project(&p).map(|p| p.name).unwrap_or_else(|_| HUB_PROJECT.into()) }
    /// The session limits on a host for a person (T-285, T-286; plans/multi-session.md § 6): working sessions per
    /// person, per provider on it, and whether the host's owner may exceed them. The host's own settings (its hello, or
    /// what its owner set from the page) first; else config workSessions { perUser 5, perProvider 3 }.
    pub fn session_limits(&self, ai: &crate::ai_hosts::AiHosts, host: &str, user: &str, prov: &str) -> (u64, u64, bool) {
        let c = self.cfg.get("workSessions").cloned().unwrap_or(json!({}));
        let (du, dp) = (c["perUser"].as_u64().filter(|n| *n > 0).unwrap_or(5), c["perProvider"].as_u64().filter(|n| *n > 0).unwrap_or(3));
        let Some(l) = (if host == "hub" { None } else { ai.session_limits(host) }) else { return (du, dp, true) };
        let short = prov.rsplit('/').next().unwrap_or(prov);
        let per_user = l["exceptions"][user].as_u64().or(l["perUser"].as_u64()).filter(|n| *n > 0).unwrap_or(du);
        let per_prov = l["providers"][short].as_u64().or(l["perProvider"].as_u64()).filter(|n| *n > 0).unwrap_or(dp);
        (per_user, per_prov, l["ownerBypass"] != json!(false))
    }
    /// How many of this person's sessions work on that host now, and how many of them through that provider.
    pub fn working_on(&self, u: &str, host: &str, prov: &str) -> (u64, u64) {
        let pre = format!("{u}#"); let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        let mine: Vec<&Run> = runs.iter().filter(|(k, _)| *k == u || k.starts_with(&pre)).map(|(_, r)| r).collect();
        (mine.iter().filter(|r| sessions::host_of(&r.prov) == host).count() as u64, mine.iter().filter(|r| r.prov == prov).count() as u64)
    }
    /// How long a run may send nothing of its own before it counts as stalled (config workSessions.stallAfterMs).
    pub fn stall_after(&self) -> u64 { self.cfg.get("workSessions").and_then(|w| w["stallAfterMs"].as_u64()).filter(|x| *x >= 10_000).unwrap_or(720_000) }
    /// Is this a session with no project (T-238: the AI only, in the person's scratch folder)?
    pub fn is_none(&self, a: &Account, sx: &Sx) -> bool { sessions::sstate(self, a, sx)["noProject"] == json!(true) }
    /// Where a session's run happens: the person's worktree of its project (the first session's own, the others' their
    /// own <user>.<id>), or the scratch folder of a session with no project.
    pub fn session_wt(&self, a: &Account, sx: &Sx) -> Result<Wt, String> {
        if self.is_none(a, sx) { return self.wt.scratch(&a.username); }
        let pj = self.project_in(a, sx);
        if sx.primary { self.wt.ensure(&a.username, &pj) } else { self.wt.ensure_session(&a.username, sx.hex(), &pj) }
    }
    pub fn project_of(&self, a: &Account) -> String { let st = self.state(a); if st["noProject"] == json!(true) { return NO_PROJECT.into(); } let p = s(&st, "project"); self.wt.project(&p).map(|p| p.name).unwrap_or_else(|_| HUB_PROJECT.into()) }
    pub fn project_list(&self) -> Value {
        let mut c = self.projects.lock().unwrap_or_else(|e| e.into_inner());
        if now_ms() - c.0 > 5000 { *c = (now_ms(), json!(self.wt.list())); }
        c.1.clone()
    }
    pub fn projects_changed(&self) { self.projects.lock().unwrap_or_else(|e| e.into_inner()).0 = 0; }
    fn model_ids(&self) -> Vec<String> { self.cfg.get("models").and_then(Value::as_array).map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default() }

    // ---------- providers (bridge/providers.js, read here) ----------
    fn hub_claude(&self) -> bool { self.cfg.get("hubClaude") != Some(&json!(false)) }
    fn host_entry(&self) -> Value { json!({ "id": "host", "kind": "host", "label": "Host Claude (default)", "owner": "", "shared": true, "models": self.model_ids() }) }
    /// bridge/providers.json; a custom endpoint (openai) or a Claude config folder on this machine (claude-cli) counts
    /// only while its owner is an admin (only admins with an AI host of their own may make one: openai.rs)
    fn own_providers(&self) -> Vec<Value> {
        let def = self.cfg.get("defaultUsername").and_then(Value::as_str).unwrap_or("mills").to_string();
        read_json(&self.root.join("providers.json")).and_then(|v| v["providers"].as_array().cloned()).unwrap_or_default().into_iter()
            .filter(|p| !crate::openai::admin_only(p) || pgm_identity_internal::admin_in(&self.root, &def, p["owner"].as_str().unwrap_or(""))).collect()
    }
    pub fn provider_get(&self, ai: &crate::ai_hosts::AiHosts, id: &str, user: &str) -> Option<Value> {
        if id.is_empty() || id == "host" { return if self.hub_claude() { Some(self.host_entry()) } else { None }; }
        if id.starts_with("h:") { return ai.provider(id, user); }
        self.own_providers().into_iter().find(|p| p["id"] == json!(id)).filter(|p| user.is_empty() || p["shared"] == json!(true) || p["owner"] == json!(user))
    }
    /// Config defaultProvider "host:<user>" → that person's online Claude host, else this hub's own Claude, else the
    /// first Claude a host offers them.
    pub fn default_for(&self, ai: &crate::ai_hosts::AiHosts, user: &str) -> Option<Value> {
        let claudes: Vec<Value> = ai.providers_for(user).into_iter().filter(|p| p["kind"] == json!("host-claude")).collect();
        let usable = |p: &&Value| p["online"] == json!(true) && p["capped"].as_str().unwrap_or("").is_empty();
        if let Some(w) = self.cfg.get("defaultProvider").and_then(Value::as_str).and_then(|d| d.strip_prefix("host:")) {
            if let Some(p) = claudes.iter().filter(|x| x["owner"] == json!(w)).find(usable).or_else(|| claudes.iter().find(|x| x["owner"] == json!(w))) { return Some(p.clone()); }
        }
        if self.hub_claude() { return Some(self.host_entry()); }
        claudes.iter().find(usable).or(claudes.first()).cloned()
    }
    pub fn effective(&self, ai: &crate::ai_hosts::AiHosts, user: &str, chosen: &str) -> Option<Value> { (if chosen.is_empty() { None } else { self.provider_get(ai, chosen, user) }).or_else(|| self.default_for(ai, user)) }
    /// What a run's `claude` gets for that provider (None: not Claude Code).
    fn claude_env(p: &Value) -> Option<Vec<(String, String)>> {
        match p["kind"].as_str().unwrap_or("") { "host" | "host-claude" => Some(vec![]), "anthropic" => Some(vec![("ANTHROPIC_API_KEY".into(), s(p, "apiKey"))]), "claude-cli" => Some(vec![("CLAUDE_CONFIG_DIR".into(), s(p, "configDir"))]), _ => None }
    }
    /// Which coding harness runs Work for that provider: "claude" (Claude Code), "jcode" (Jcode PGM), None (neither:
    /// an OpenAI endpoint or a ChatGPT login answers the API and the Unity section, not Work).
    pub fn harness_kind(p: &Value) -> crate::harness::HarnessKind { if p["kind"] == json!(crate::jcode::KIND) || p["kind"] == json!(HOST_JCODE) { crate::harness::HarnessKind::Jcode } else { crate::harness::HarnessKind::Claude } }
    pub fn harness_of(p: &Value) -> Option<&'static str> {
        if p["kind"] == json!(crate::jcode::KIND) || p["kind"] == json!(HOST_JCODE) { return Some(crate::jcode::HARNESS); }
        Self::claude_env(p).map(|_| crate::jcode::CLAUDE_HARNESS)
    }
    fn usage_source(&self, ai: &crate::ai_hosts::AiHosts, a: &Account) -> String { match self.effective(ai, &a.username, &s(&self.state(a), "provider")) { Some(p) if p["kind"] == json!("host-claude") => s(&p, "id"), _ => "hub".into() } }
    pub fn usage_status(&self, hub: &dyn AiHub, a: &Account) -> Value {
        let st = self.state(a);
        let mut v = hub.ai().usage.status(&self.usage_source(hub.ai(), a), &st["ctx"], st["claudeSessionId"].as_str().unwrap_or(""));
        // answers outside Work (API, Unity, products): the cache ledger of this person's answers; absent until there is one
        let ans = hub.ai().usage.answers(&a.username);
        if ans.is_object() { v["answers"] = ans; }
        v
    }

    // ---------- the person's Forgejo token (bridge/forgejo.js keeps it) ----------
    fn forgejo_url(&self) -> String { read_json(&self.root.join("forgejo.json")).and_then(|c| c["url"].as_str().map(String::from)).or_else(|| self.cfg.get("forgejoUrl").and_then(Value::as_str).map(String::from)).unwrap_or_else(|| "https://git.unityailab.com".into()).trim_end_matches('/').to_string() }
    fn forgejo_rec(&self, u: &str) -> Option<Value> { read_json(&self.root.join("forgejo-users.json")).and_then(|v| v["users"].get(u).cloned()).filter(Value::is_object) }
    pub fn forgejo_login(&self, u: &str) -> Option<String> { self.forgejo_rec(u).map(|r| s(&r, "login")).filter(|l| !l.is_empty()) }
    pub fn work_env(&self, u: &str) -> Vec<(String, String)> {
        let Some(r) = self.forgejo_rec(u) else { return vec![] };
        use base64::Engine;
        let url = self.forgejo_url(); let n: u64 = std::env::var("GIT_CONFIG_COUNT").ok().and_then(|x| x.parse().ok()).unwrap_or(0);
        vec![("FORGEJO_URL".into(), url.clone()), ("FORGEJO_TOKEN".into(), s(&r, "token")), ("FORGEJO_LOGIN".into(), s(&r, "login")), ("GIT_CONFIG_COUNT".into(), (n + 1).to_string()),
            (format!("GIT_CONFIG_KEY_{n}"), format!("http.{url}/.extraHeader")), (format!("GIT_CONFIG_VALUE_{n}"), format!("Authorization: Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", s(&r, "login"), s(&r, "token")))))]
    }

    // ---------- worktree freshness (the worktree law): checked in the background, never holding a run up ----------
    pub fn check_fresh(self: &Arc<Self>, a: &Account) { let pj = self.project_of(a); self.check_fresh_in(a, &pj, None); }
    /// The background freshness check of a worktree: the person's own (None) or one of their sessions' (its id's hex).
    pub fn check_fresh_in(self: &Arc<Self>, a: &Account, pj: &str, hex: Option<&str>) {
        if pj == NO_PROJECT { return; }
        let pj = pj.to_string(); let hex = hex.map(String::from); let key = fresh_key(&a.username, &pj, hex.as_deref());
        { let mut f = self.fresh.lock().unwrap_or_else(|e| e.into_inner()); let cur = f.get(&key).cloned().unwrap_or_default(); if cur.busy || (cur.at > 0 && now_ms() - cur.at < 60000) { return; } f.insert(key.clone(), Fresh { busy: true, ..cur }); }
        let (w, user) = (self.clone(), a.username.clone());
        tokio::task::spawn_blocking(move || {
            let mut env = w.work_env(&user); env.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
            let mut args: Vec<&str> = vec!["--project", &pj, "auto", &user]; if let Some(h) = &hex { args.push("--session"); args.push(h); }
            let (_, out, err) = w.wt.tool(&args, &env);
            let raw = if out.trim().is_empty() { err } else { out };
            let lines: Vec<&str> = raw.trim().lines().collect();
            let text: String = lines[lines.len().saturating_sub(2)..].join(" ").chars().take(600).collect();
            w.fresh.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Fresh { text, at: now_ms(), busy: false });
        });
    }
    fn fresh_law(&self, a: &Account, wd: &Wt) -> String {
        let script = self.wt.script.display().to_string().replace('\\', "/");
        let cmd = if wd.hub { "node bridge/worktree.js".to_string() } else { format!("node {script} --project {}", wd.project) };
        let u = &a.username;
        if let Some(h) = session_of(u, wd) { return format!("Worktree law: never build on a stale worktree. This session's worktree builds on {u}'s own work/{u} (never touch that branch from here). Before changing anything, check it: {cmd} check {u} --session {h} (exit code 3 = stale: work/{u} has moved on). If it is STALE, run {cmd} update {u} --session {h}: it fast-forwards or merges work/{u} into this branch. When {u} asks to bring the work home: {cmd} home {u} --session {h} (it merges this branch into work/{u} in their own worktree). The latest background check of this worktree is at the end of each message."); }
        // the law itself is the same every run (it stays in the cached system prefix); the latest check result is
        // volatile ("N min ago") and travels with the turn (fresh_note), so it never busts the prompt cache
        format!("Worktree law: never build on a stale worktree. Before changing anything, check it: {cmd} check {u} (it fetches Forgejo first; exit code 3 = stale). If it is STALE, run {cmd} update {u}: it fast-forwards, merges Forgejo's {} into your own commits, or makes a NEW worktree from Forgejo with your commits, uncommitted changes and untracked files carried over and then deletes just the old one ({cmd} renew {u} does that directly). The latest background check of this worktree is at the end of each message.", wd.main)
    }
    /// The latest background freshness check, for the turn (not the system prompt: pgm-context's lint would flag the
    /// "N min ago" there, and every run would pay the whole system prompt again).
    fn fresh_note(&self, a: &Account, wd: &Wt) -> String {
        let f = self.fresh.lock().unwrap_or_else(|e| e.into_inner()).get(&fresh_key(&a.username, &wd.project, session_of(&a.username, wd).as_deref())).cloned();
        f.filter(|f| !f.text.is_empty()).map(|f| format!("[Worktree: latest background check ({} min ago): {}]", ((now_ms() - f.at) as f64 / 60000.0).round().max(1.0), f.text)).unwrap_or_default()
    }
    fn hub_laws(&self) -> String { ["LAWS.md", "CLAUDE.md"].iter().filter_map(|f| std::fs::read_to_string(self.mills.join(f)).ok().map(|t| t.trim().to_string())).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n") }
    /// The stable segments of the Work system prompt (pgm-context assembly): the person's own lines are the same for the
    /// whole session (Session), the laws the same for everyone (Static). Nothing volatile may be in them (the lint).
    pub fn system_segments(&self, a: &Account, wd: &Wt) -> Vec<Segment> {
        let (name, u) = (&a.display, &a.username);
        let script = self.wt.script.display().to_string().replace('\\', "/");
        let dir = wd.dir.display(); let b = &wd.branch;
        let wher: Vec<String> = if wd.project == NO_PROJECT { vec![
            format!("{name} started this session with NO PROJECT: there is no git repository and no worktree. Your working folder is {dir}, a scratch folder that only {name} uses on this hub (nothing in it is shared, versioned or backed up)."),
            "Use it for notes, drafts, experiments and one-off scripts; never touch the VibeRoller checkout, any project's folders or other people's folders. The worktree law (check / update / merge) is for real projects and does not apply here.".to_string(),
            format!("If the work turns into a project, tell {name}: Work's Start work panel starts a new private project on Forgejo or clones one, in their own worktree; then copy over what is worth keeping."),
        ] } else if wd.hub { vec![
            format!("For work on the VibeRoller project itself you are in {name}'s OWN git worktree: {dir}, branch {b}."),
            format!("Commit only on {b}; never check out other branches here and never touch other people's worktrees in {}.", self.wt.wt_root.display()),
            "Before committing, run the regression suite from the worktree: node bridge/test/regress.js --no-prod (it must report 0 failed).".into(),
            if wd.branch != format!("work/{u}") { format!("This is one of {name}'s Work sessions with its own worktree and branch (others may run in parallel in theirs). Only when {name} asks to bring it home: commit here, then in their own worktree {} run git merge {b}, and from there, when they ask to merge into develop, node bridge/worktree.js commit {u} \"<message>\" then node bridge/worktree.js merge {u}", wd.root.join(u.as_str()).display()) }
            else { format!("Only when {name} asks you to merge into develop, run from the worktree: node bridge/worktree.js commit {u} \"<message>\" then node bridge/worktree.js merge {u}") },
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
        let intro = vec![
            format!("You are talking with {name} (username \"{u}\"{}, Unity AI Lab engineer) through the VibeRoller web interface, running on the host's Claude account.", if a.admin { ", admin" } else { "" }),
            format!("Every message in this conversation comes from {name}; address them by name and keep their work separate from everyone else's."),
        ];
        let mut parts: Vec<String> = vec![];
        parts.extend([
            format!("{name} has opened a folder on THEIR OWN PC in their browser. To build or edit anything on their PC, use the mcp__pc__* tools"),
            format!("(list, read, write, delete, mkdir) with paths relative to that folder. Your built-in file tools act on the host's VibeRoller folder, not {name}'s PC."),
            format!("Shell commands cannot run on {name}'s PC; if something needs running there, tell {name} exactly what to run."),
            format!("The VibeRoller team board (shared with everyone and with other Claude runs) is available through the mcp__board__* tools, where you act as \"{name} · Claude\"."),
            "When you work a board task: task_claim it first, task_renew while working longer than the claim, add progress with task_update appendNote, and task_complete or task_release it when you stop. Never edit a task someone else holds.".into(),
            "You may plan with the board and calendar when asked: board_calendar shows tasks by due day, task_create / task_update take due as text (today, tomorrow, +3d, next friday, YYYY-MM-DD, none), task_move puts a task in a column, task_delete removes one (only when asked), board_new makes a board. A task with a due date IS the calendar entry.".into(),
            format!("Beyond chat: hub_status (who is here, calls, shared Work sessions, the host's usage windows), sessions_search ({name}'s past Claude sessions), my_providers (the AIs available), team_channel_create / team_group_create."),
        ]);
        if self.forgejo_rec(u).map(|r| !s(&r, "token").is_empty()).unwrap_or(false) { parts.push(format!("Forgejo ({}): {name} saved their own token, so git pushes / fetches to that host authenticate as them, FORGEJO_URL / FORGEJO_TOKEN are in your environment for its API (never print the token), and fj (the Forgejo CLI) is signed in as them in this run. node bridge/worktree.js repos [words] lists the repos they can see (private ones too), repo-create <owner/name> makes one, publish <owner/name> --project <name> puts a project on Forgejo (a GitHub origin kept as upstream).", self.forgejo_url())); }
        parts.push("Team notes (markdown files in folders, shared by everyone): notes_list (with a search), notes_read, notes_write (path like \"plans/next-week.md\"; whole text), notes_delete (only when asked). Good for plans, meeting notes, decisions, how-tos.".into());
        parts.push(format!("The team chat is reachable through the mcp__team__* tools: team_say posts a message to # Everyone (or privately with to=<username>) as \"{name} · Claude\", team_presence tells you who is online. Announce in the chat BEFORE any hub restart and when you finish something the team should know about."));
        vec![
            Segment::new("1-intro", Class::Session, intro.join(" ")),
            Segment::new("2-where", Class::Session, wher.join(" ")),
            Segment::new("3-tools", Class::Session, parts.join(" ")),
            Segment::new("4-laws", Class::Static, self.hub_laws()),
        ]
    }
    /// The system prompt a run gets: the segments joined (the person's lines with a space, the laws after a blank line).
    /// The same bytes every run, so the provider's prompt cache can hit (R181); logs any lint hit once per hub process.
    fn system_prompt(&self, a: &Account, wd: &Wt) -> String {
        let segs = self.system_segments(a, wd);
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !DONE.swap(true, Ordering::SeqCst) {
            for h in lint_segments(&segs) { eprintln!("[work] system prompt segment \"{}\" ({:?}) has {}: \"{}\"; it busts the prompt cache, move it to the turn (R181)", h.segment, h.class, h.pattern, h.snippet); }
        }
        render_prompt(&segs)
    }
    /// The MCP servers of a run here: PC files, the board, the team chat (bridge/*-mcp.js), with this hub's secret.
    fn mcp_json(&self, a: &Account, sx: &Sx) -> Value {
        let node = std::env::var("VR_NODE").unwrap_or_else(|_| "node".into());
        let mut env = json!({ "MILLS_BRIDGE": format!("http://127.0.0.1:{}", self.cfg.port), "MILLS_INTERNAL": self.secret_for(&a.username), "MILLS_USER": a.display, "MILLS_ACCOUNT": a.username });
        if !sx.primary { env["MILLS_SESSION"] = json!(sx.sid); } // its PC tools reach the page of THIS session's run
        let srv = |js: &str| json!({ "command": node, "args": [std::path::absolute(self.root.join(js)).unwrap_or_default().display().to_string()], "env": env });
        json!({ "mcpServers": { "pc": srv("pc-tools-mcp.js"), "board": srv("board-mcp.js"), "team": srv("team-mcp.js") } })
    }
    fn mcp_config(&self, a: &Account, sx: &Sx) -> PathBuf {
        let f = if sx.primary { a.dir.join(".mcp-runtime-hub.json") } else { a.dir.join(format!(".mcp-runtime-hub-{}.json", sx.hex())) };
        write_json(&f, &self.mcp_json(a, sx));
        pgm_core_internal::fs::private(&f);
        f
    }
    fn repo_url_for(&self, wd: &Wt) -> String {
        if wd.hub { return self.cfg.get("hubRepoUrl").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| pgm_collab_internal::worktree::git(&self.mills, &["remote", "get-url", "origin"]).unwrap_or_default()); }
        self.cfg.get("projectsGitBase").and_then(Value::as_str).map(|b| format!("{}/{}/_repo", b.trim_end_matches('/'), wd.project)).unwrap_or_default()
    }
    // ---------- the login relay: one token per person + host provider, 12 hours, from this machine only ----------
    fn llm_token(&self, user: &str, host: &str, prov: &str) -> String {
        let mut m = self.llm.lock().unwrap_or_else(|e| e.into_inner()); let now = now_ms();
        m.retain(|_, v| v.3 > now);
        if let Some((t, _)) = m.iter().find(|(_, v)| v.0 == user && v.1 == host && v.2 == prov && v.3 - now > 3_600_000) { return t.clone(); }
        let t = pgm_core_internal::crypto::random_hex(24);
        m.insert(t.clone(), (user.into(), host.into(), prov.into(), now + 12 * 3_600_000));
        t
    }

    // ---------- the run's stream ----------
    fn emit(&self, key: &str, ev: Value) { if let Some(r) = self.runs.lock().unwrap_or_else(|e| e.into_inner()).get_mut(key) { if ev["type"] != json!("ping") { r.last = now_ms(); } let _ = r.tx.send(line(ev.clone())); r.ev.push(ev); } }
    fn on_event(&self, hub: &dyn AiHub, a: &Account, sx: &Sx, ev: Value, source: &str) {
        // every harness event (here, or from an AI host's run, whichever harness made it) leaves as HubEvent: the alias
        // fields as they came plus the `hub` object the page reads first (harness.rs, WORKPLAN 9b)
        let ev = crate::harness::wire(ev);
        if let Some(sid) = ev["session_id"].as_str().filter(|s| !s.is_empty()) { let sid = sid.to_string(); sessions::supdate(self, a, sx, |m| { m.insert("claudeSessionId".into(), json!(sid)); }); }
        let usage = &hub.ai().usage;
        let t = ev["type"].as_str().unwrap_or("");
        if !source.is_empty() { usage.note_event(&ev, source); } else if !t.contains("rate_limit") { usage.note_event(&ev, "hub"); }
        if t == "assistant" || t == "result" {
            let st = sessions::sstate(self, a, sx); let mut ctx = st["ctx"].clone(); let before = ctx.clone();
            crate::usage::note_context(&mut ctx, &ev, st["claudeSessionId"].as_str().unwrap_or(""));
            if ctx != before { sessions::supdate(self, a, sx, |m| { m.insert("ctx".into(), ctx); }); }
        }
        if t == "result" {
            if let Some(r) = self.runs.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&sx.key) { r.answered = true; }
            // it stopped short of a tool it was not allowed: the session waits on its person (yellow, T-288) until they answer
            let denied: Vec<String> = ev["permission_denials"].as_array().map(|d| d.iter().filter_map(|x| x["tool_name"].as_str().map(String::from)).collect()).unwrap_or_default();
            if !denied.is_empty() { let now = now_ms(); sessions::touch(a, &sx.sid, |m| { m.insert("waiting".into(), json!({ "kind": "permission", "at": now, "tools": denied })); }); }
        }
        if self.shares_session(&a.username, &sx.sid) { self.share_event(&a.username, &ev); } // only the session that is shared (plans/multi-session.md § 7)
        self.emit(&sx.key, ev);
    }
    fn finish(&self, a: &Account, sx: &Sx, run_id: &str, code: Value) {
        let run = { let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner()); if runs.get(&sx.key).map(|r| r.id == run_id).unwrap_or(false) { runs.remove(&sx.key) } else { None } };
        let Some(mut r) = run else { return };
        for (_, p) in r.pending { let _ = p.send(json!({ "ok": false, "error": "Run ended" })); }
        self.ask_end(a, sx); // its open questions end with it (the session is no longer waiting on them)
        let done = json!({ "type": "bridge_done", "code": code });
        let _ = r.tx.send(line(done.clone()));
        r.ev.push(done);
        // a page reloading just now still sees how it ended (for a few minutes)
        self.ended.lock().unwrap_or_else(|e| e.into_inner()).insert(sx.key.clone(), (r.id.clone(), r.ev, now_ms()));
        // the session's dot: red (done, not seen yet) when it answered
        let answered = r.answered; let now = now_ms();
        sessions::touch(a, &sx.sid, |m| { if answered { m.insert("lastReplyAt".into(), json!(now)); } m.insert("lastActivityAt".into(), json!(now)); });
        self.busy_changed.notify_one();
    }
    /// GET /api/work/attach?run=<id>&after=<n>: the person's run so far and as it goes (a long poll, 25 s): the events
    /// after the n-th, whether it is still going, and its id. A page that was reloaded joins its run this way. A PC tool
    /// request goes out again only while it still waits for an answer (never run twice).
    async fn attach(&self, u: &str, run: &str, after: usize) -> Value {
        let view = |w: &Work| -> (Value, bool, Option<Arc<tokio::sync::Notify>>) {
            if let Some(r) = w.runs.lock().unwrap_or_else(|e| e.into_inner()).get(u) {
                let from = if r.id == run { after } else { 0 };
                let evs: Vec<Value> = r.ev.since(from).into_iter().filter(|e| e["type"] != json!("pc_tool") || r.pending.contains_key(e["id"].as_str().unwrap_or(""))).collect();
                let ready = !evs.is_empty() || r.id != run;
                return (json!({ "run": r.id, "busy": true, "from": from, "next": r.ev.next(), "events": evs }), ready, Some(r.ev.woke.clone()));
            }
            let mut ended = w.ended.lock().unwrap_or_else(|e| e.into_inner());
            ended.retain(|_, x| now_ms() - x.2 < 5 * 60_000);
            match ended.get(u) {
                Some((id, ev, _)) if id == run => { let evs: Vec<Value> = ev.since(after).into_iter().filter(|e| e["type"] != json!("pc_tool")).collect(); (json!({ "run": id, "busy": false, "from": after, "next": ev.next(), "events": evs }), true, None) }
                _ => (json!({ "run": "", "busy": false, "events": [] }), true, None),
            }
        };
        let (v, ready, woke) = view(self);
        if ready { return v; }
        let Some(woke) = woke else { return v };
        let n = woke.notified(); tokio::pin!(n); n.as_mut().enable();
        let (v, ready, _) = view(self); if ready { return v; }
        let _ = tokio::time::timeout(Duration::from_secs(25), n).await;
        view(self).0
    }
    /// Stops the person's run: the process here, or the host is told.
    pub fn stop(&self, hub: &dyn AiHub, u: &str) {
        let (pid, remote) = match self.runs.lock().unwrap_or_else(|e| e.into_inner()).get(u) { Some(r) => (r.pid, r.remote.clone()), None => return };
        if !remote.is_empty() { hub.ai().cancel_work(&remote); return; }
        if let Some(pid) = pid { kill_pid(pid); }
    }

    // ---------- the AI asks its person (T-245; team note plans/question-protocol.md) ----------
    /// An open question keeps its run alive for the liveness check (a person may take longer than stallAfter).
    fn keep_alive(&self, key: &str) { if let Some(r) = self.runs.lock().unwrap_or_else(|e| e.into_inner()).get_mut(key) { r.last = now_ms(); } }
    /// The session's dot: waiting (yellow) while any question of it is open.
    fn ask_waiting(&self, a: &Account, sid: &str) {
        let open: Vec<(u64, String)> = self.asks.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, x)| x.user == a.username && x.sid == sid && x.result.is_none()).map(|(id, x)| (x.at, id.clone())).collect();
        let first = open.iter().min().cloned();
        sessions::touch(a, sid, |m| {
            match first {
                Some((at, id)) => { m.insert("waiting".into(), json!({ "kind": "question", "at": at, "ref": id, "count": open.len() })); }
                None => { if m.get("waiting").map(|w| w["kind"] == json!("question")).unwrap_or(false) { m.remove("waiting"); } }
            }
        });
    }
    /// Answers a question (the person, the timeout, or the end of the run): the waiting tool call gets `result`. False = it was not open.
    fn settle(&self, a: &Account, id: &str, result: Value) -> bool {
        let (key, sid) = { let mut m = self.asks.lock().unwrap_or_else(|e| e.into_inner()); let Some(x) = m.get_mut(id) else { return false }; if x.result.is_some() { return false; } let by = result["result"]["by"].clone(); let answers = result["result"]["answers"].clone(); x.result = Some(result); x.notify.notify_waiters(); (x.key.clone(), (x.sid.clone(), by, answers)) };
        let (sid, by, answers) = sid;
        self.ask_waiting(a, &sid);
        self.emit(&key, json!({ "type": "bridge_answered", "id": id, "session": sid, "by": by, "answers": answers }));
        true
    }
    /// A run ended: its open questions end with it.
    fn ask_end(&self, a: &Account, sx: &Sx) {
        let mut any = false;
        { let mut m = self.asks.lock().unwrap_or_else(|e| e.into_inner());
          let mine: Vec<String> = m.iter().filter(|(_, x)| x.key == sx.key).map(|(id, _)| id.clone()).collect();
          for id in mine { let open = m.get(&id).map(|x| x.result.is_none()).unwrap_or(false); if open { any = true; if let Some(x) = m.get_mut(&id) { x.result = Some(json!({ "ok": false, "error": "Run ended" })); x.notify.notify_waiters(); } } else { m.remove(&id); } } }
        if any { self.ask_waiting(a, &sx.sid); }
    }
    /// The tool `ask_user` (team MCP): opens a question series for the session that asked and waits for the answers; or,
    /// with `poll: <id>`, waits on one already open (the MCP relay asks again every few minutes, so no HTTP call is held for 10).
    async fn ask(&self, a: &Account, b: &Value, args: &Value) -> Value {
        let err = |e: String| json!({ "ok": false, "error": e });
        let poll = args["pollSec"].as_u64().unwrap_or(240).clamp(1, 240);
        if let Some(id) = args["poll"].as_str() { return self.ask_wait(a, id, poll).await; }
        let (title, qs) = match questions::normalize(args) { Ok(x) => x, Err(e) => return err(format!("ask_user: {e}")) };
        let want = s(b, "session");
        let sx = sessions::resolve(a, &want).filter(|sx| !want.is_empty() && self.busy_key(&sx.key))
            .or_else(|| self.run_key_of(&a.username).and_then(|k| sessions::resolve(a, k.split_once('#').map(|x| x.1).unwrap_or(""))));
        let Some(sx) = sx else { return err("No run of yours is going, so there is nobody to ask.".into()) };
        // the wait: the tool's own timeoutSec, else the person's setting (prefs.ask: auto off = never, or minutes), else config, else 10 minutes
        let person = read_json(&a.dir.join("state.json")).map(|st| st["prefs"]["ask"].clone()).filter(Value::is_object).and_then(|p| if p["auto"] == json!(false) { Some(0) } else { p["minutes"].as_f64().filter(|m| *m >= 1.0).map(|m| (m.min(1440.0) * 60.0) as u64) });
        let sec = questions::timeout_sec(args, person.unwrap_or_else(|| self.cfg.get("questionTimeoutSec").and_then(Value::as_u64).unwrap_or(questions::DEFAULT_TIMEOUT_SEC)));
        let now = now_ms(); let id = format!("q-{}", pgm_core_internal::crypto::random_hex(6));
        let deadline = if sec > 0 { now + sec * 1000 } else { u64::MAX };
        {
            let mut m = self.asks.lock().unwrap_or_else(|e| e.into_inner());
            m.retain(|_, x| now.saturating_sub(x.at) < 2 * 86_400_000);
            if m.values().any(|x| x.key == sx.key && x.result.is_none()) { return err("A question of yours is still waiting for its answer in this session; wait for it before asking another.".into()); }
            m.insert(id.clone(), Ask { user: a.username.clone(), key: sx.key.clone(), sid: sx.sid.clone(), title: title.clone(), qs: qs.clone(), at: now, timeout_ms: sec * 1000, deadline, result: None, notify: Arc::new(Notify::new()) });
        }
        self.ask_waiting(a, &sx.sid);
        self.emit(&sx.key, json!({ "type": "bridge_question", "id": id, "session": sx.sid, "title": title, "questions": qs, "timeoutSec": sec, "at": now, "deadline": if sec > 0 { json!(deadline) } else { Value::Null } }));
        self.ask_wait(a, &id, poll).await
    }
    async fn ask_wait(&self, a: &Account, id: &str, poll: u64) -> Value {
        let end = std::time::Instant::now() + Duration::from_secs(poll);
        let gone = || json!({ "ok": false, "error": "No such open question (it was answered, or the run ended)." });
        loop {
            let Some(notify) = self.asks.lock().unwrap_or_else(|e| e.into_inner()).get(id).filter(|x| x.user == a.username).map(|x| x.notify.clone()) else { return gone() };
            let nf = notify.notified(); tokio::pin!(nf); nf.as_mut().enable(); // before the look below: an answer in between still wakes us
            let snap = self.asks.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|x| (x.result.clone(), x.key.clone(), x.timeout_ms, x.deadline));
            let Some((res, key, tmo, deadline)) = snap else { return gone() };
            if let Some(r) = res { self.asks.lock().unwrap_or_else(|e| e.into_inner()).remove(id); return r; }
            self.keep_alive(&key);
            let now = now_ms();
            if tmo > 0 && now >= deadline {
                let qs = self.asks.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|x| x.qs.clone()).unwrap_or_default();
                self.settle(a, id, json!({ "ok": true, "result": { "answered": true, "by": "default", "timedOut": true, "answers": questions::default_answers(&qs) } }));
                continue;
            }
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() { return json!({ "ok": true, "result": { "pending": true, "id": id } }); }
            let mut wait = left.min(Duration::from_secs(2));
            if tmo > 0 { wait = wait.min(Duration::from_millis(deadline.saturating_sub(now)).max(Duration::from_millis(1))); }
            let _ = tokio::time::timeout(wait, nf).await;
        }
    }
    /// POST /api/work/answer {id, answers: [{id, picked, other?, text?}]} (or {id, defaults: true}, or {id, activity: true}
    /// = the person is busy with it: the timeout starts over).
    fn answer(&self, a: &Account, b: &Value) -> (u16, Value) {
        let id = s(b, "id");
        let snap = self.asks.lock().unwrap_or_else(|e| e.into_inner()).get(&id).filter(|x| x.user == a.username).map(|x| (x.result.is_some(), x.qs.clone(), x.key.clone(), x.result.as_ref().map(|r| r["ok"] == json!(false)).unwrap_or(false)));
        let Some((done, qs, key, ended)) = snap else { return (404, json!({ "error": "No such open question." })) };
        if ended { return (404, json!({ "error": "No such open question (its run ended)." })); }
        if done { return (409, json!({ "error": "That question was answered already." })); }
        let yes = |k: &str| b[k] == json!(true) || b[k] == json!(1);
        if yes("activity") {
            let mut m = self.asks.lock().unwrap_or_else(|e| e.into_inner());
            let dl = m.get_mut(&id).map(|x| { if x.timeout_ms > 0 { x.deadline = now_ms() + x.timeout_ms; } x.deadline }).unwrap_or(0);
            drop(m); self.keep_alive(&key);
            return (200, json!({ "ok": true, "deadline": if dl == u64::MAX { Value::Null } else { json!(dl) } }));
        }
        let answers = if yes("defaults") { questions::default_answers(&qs) } else { questions::answers_from(&qs, &b["answers"]) };
        let list = answers.as_array().cloned().unwrap_or_default();
        let by = if list.iter().all(|x| x["by"] == json!("user")) { "user" } else if list.iter().all(|x| x["by"] == json!("default")) { "default" } else { "mixed" };
        if !self.settle(a, &id, json!({ "ok": true, "result": { "answered": true, "by": by, "answers": answers } })) { return (409, json!({ "error": "That question was answered already." })); }
        (200, json!({ "ok": true, "by": by }))
    }
    /// The questions of theirs that wait now (a page that was away asks), of one session when named.
    fn open_questions(&self, a: &Account, session: &str) -> Vec<Value> {
        let want = if session.is_empty() { String::new() } else { session.to_string() };
        let mut v: Vec<(u64, Value)> = self.asks.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, x)| x.user == a.username && x.result.is_none() && (want.is_empty() || x.sid == want))
            .map(|(id, x)| (x.at, json!({ "id": id, "session": x.sid, "title": x.title, "questions": x.qs, "timeoutSec": x.timeout_ms / 1000, "at": x.at, "deadline": if x.timeout_ms > 0 { json!(x.deadline) } else { Value::Null } }))).collect();
        v.sort_by_key(|x| x.0); v.into_iter().map(|x| x.1).collect()
    }
}
/// Asks a process to stop (pgm-platform-internal since 5.0.0).
pub use pgm_platform_internal::proc::kill_pid;

fn ndjson(rx: mpsc::UnboundedReceiver<Result<Bytes, String>>) -> hyper::Response<pgm_net_internal::http::Body> {
    use http_body_util::BodyExt;
    let st = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x.map(hyper::body::Frame::data).map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e)), rx)) });
    hyper::Response::builder().status(200).header("content-type", "application/x-ndjson").header("cache-control", "no-cache").header("x-accel-buffering", "no").body(http_body_util::StreamBody::new(st).boxed()).expect("response")
}
fn one_line(v: Value) -> Resp { hyper::Response::builder().status(200).header("content-type", "application/x-ndjson").body(pgm_net_internal::http::full(v.to_string() + "\n")).expect("response") }

/// POST /api/chat: one message to Claude in Work.
async fn chat(hub: &Arc<dyn AiHub>, a: &Account, r: &Req) -> Resp {
    let w = hub.work();
    let b = r.json();
    // the session it goes to (plans/multi-session.md; none named = the first one, as older pages send it)
    let Some(sx) = sessions::resolve(a, &sessions::wanted(r, &b)) else { return answer(404, json!({ "error": "No such session." })) };
    if w.busy_key(&sx.key) { return answer(409, json!({ "error": "Claude is already working. Stop it first." })); }
    // a requested restart wins the gap between two replies (the page retries a 503 by itself)
    if std::env::var("MILLS_SUPERVISED").is_ok() && hub.root().join(".restart").exists() { return answer(503, json!({ "error": "VibeRoller is restarting; your message goes through in a moment." })); }
    let Some(message) = b["message"].as_str().filter(|m| !m.is_empty()).map(String::from) else { return answer(400, json!({ "error": "Empty message." })) };
    let context: String = b["context"].as_str().unwrap_or("").chars().take(20000).collect();
    let fresh = b["fresh"].as_bool().unwrap_or(false) || (b["fresh"].is_number() && b["fresh"] != json!(0)) || b["fresh"].as_str().map(|s| !s.is_empty()).unwrap_or(false);
    if fresh { sessions::supdate(w, a, &sx, |m| { m.remove("claudeSessionId"); m.remove("ctx"); }); }
    if w.shares_session(&a.username, &sx.sid) { w.share_line(&a.username, "user", &message); }
    let pj = w.project_in(a, &sx);
    // each session its own worktree (T-292): the first one the person's, the others <user>.<id> on work/<user>.<id>
    let none = pj == NO_PROJECT;
    let wd = match w.session_wt(a, &sx) { Ok(wd) => wd, Err(e) => return one_line(json!({ "type": "bridge_error", "text": format!("Could not prepare your worktree: {e} (switch to the shared folder or ask the host).") })) };
    sessions::supdate(w, a, &sx, |m| { m.insert("lastRunAt".into(), json!(now_ms())); });
    // a new message answers whatever the session waited for (T-288)
    { let (path, branch) = (wd.dir.display().to_string(), wd.branch.clone()); sessions::touch(a, &sx.sid, |m| { m.insert("worktree".into(), json!({ "path": path, "branch": branch, "project": wd.project })); m.insert("lastActivityAt".into(), json!(now_ms())); m.remove("ended"); m.remove("waiting"); }); }
    if none { } else if sx.primary { w.check_fresh(a); } else { w.check_fresh_in(a, &pj, Some(sx.hex())); }
    let st = sessions::sstate(w, a, &sx);
    let permission_mode: String = hub.cfg().get("permissionMode").and_then(Value::as_str).unwrap_or("default").into();
    let mcp_config = w.mcp_config(a, &sx);
    let system_prompt = w.system_prompt(a, &wd);
    let mut tools: Vec<String> = hub.cfg().get("allowedTools").and_then(Value::as_array).map(|t| t.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if !wd.hub { tools.push(format!("Bash(node {}:*)", w.wt.script.display().to_string().replace('\\', "/"))); }
    let model = { let m = s(&st, "model"); if m.is_empty() { w.default_model_of(a) } else { m } };
    let ai = hub.ai();
    let Some(prov) = w.effective(ai, &a.username, &s(&st, "provider")) else { return one_line(json!({ "type": "bridge_error", "text": "No AI is available for Work: this hub has no Claude of its own and no AI host offering one is connected. Ask the person whose AI host it is to start it (the heart), or hook up an AI of your own in Settings." })) };
    let Some(harness) = Work::harness_of(&prov) else { return one_line(json!({ "type": "bridge_error", "text": "Your chosen AI provider can run neither Claude Code nor Jcode PGM, so it cannot run Work. Pick a Claude or a Jcode PGM model in the dropdown." })) };
    let prov_env = Work::claude_env(&prov).unwrap_or_default();
    let kind = s(&prov, "kind");
    if (kind == "host" || kind == "claude-cli") && hub.root().join("ai-host.off").exists() { return one_line(json!({ "type": "bridge_error", "text": AI_OFF_TEXT })); }
    // a provider shared with a scope: Work must be among its uses and the model among its models (the owner names what is allowed)
    if let Err(m) = ai.scope_check(&a.username, &s(&prov, "id"), "work", &model) { return one_line(json!({ "type": "bridge_error", "text": format!("{m} Pick another AI or model in the dropdown.") })); }
    let owner = s(&prov, "owner"); let nick = s(&prov, "nickname");
    let host_id = s(&prov, "hostId"); let prov_id = s(&prov, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default();
    let hjcode = kind == HOST_JCODE;
    if (kind == "host-claude" || hjcode) && !ai.online(&host_id) { return one_line(json!({ "type": "bridge_error", "text": format!("{nick}{} is offline right now. Pick another AI in the dropdown, or wait for it.", if owner.is_empty() { String::new() } else { format!(" ({}'s AI host)", hub.name_of(&owner)) }) })); }
    if (kind == "host-claude" || hjcode) && !s(&prov, "capped").is_empty() { return one_line(json!({ "type": "bridge_error", "text": format!("{nick}: {}. Pick another AI or wait for the window to reset.", s(&prov, "capped")) })); }
    // an older host (no relay) would run the whole Work run on its own computer: only its owner may touch that computer
    if kind == "host-claude" && s(&prov, "remoteKind") == "claude-cli-work" && owner != a.username {
        return one_line(json!({ "type": "bridge_error", "text": format!("{nick} is an older AI host that would run Work on {} computer, and only its owner may work on that computer. Pick another AI in the dropdown, or ask them to update the host (then Work runs here and only its AI is used).", if owner.is_empty() { "its own".to_string() } else { format!("{}'s", hub.name_of(&owner)) }) }));
    }
    // Jcode PGM on a host runs Work on that host's computer, like the legacy case above: only its owner may
    if hjcode && owner != a.username {
        return one_line(json!({ "type": "bridge_error", "text": format!("{nick} runs Work through Jcode PGM on {} computer, and only its owner may work on that computer. Pick another AI in the dropdown.", if owner.is_empty() { "its own".to_string() } else { format!("{}'s", hub.name_of(&owner)) }) }));
    }
    // a session with no project works in a folder on THIS hub: a host's computer has no such folder
    if none && (hjcode || (kind == "host-claude" && s(&prov, "remoteKind") == "claude-cli-work")) {
        return one_line(json!({ "type": "bridge_error", "text": format!("{nick} runs Work on another computer, but a session with no project works in your scratch folder on this hub. Pick the hub's Claude or a Claude shared as AI only, or give this session a project.") }));
    }
    // the limits (T-285): only working sessions count, per host and per provider on it; a host's owner is not limited on it
    { let pfull = s(&prov, "id"); let host = sessions::host_of(&pfull);
      let (max_u, max_p, bypass) = w.session_limits(ai, &host, &a.username, &pfull);
      if owner.is_empty() || owner != a.username || !bypass { let (used, used_p) = w.working_on(&a.username, &host, &pfull);
        let where_ = if host == "hub" { "This hub".to_string() } else if nick.is_empty() { host.clone() } else { nick.clone() };
        if used >= max_u { return answer(429, json!({ "error": format!("{where_}: {used} of {max_u} sessions working. Wait for one, stop one, or use another AI host."), "limit": { "host": host, "used": used, "max": max_u } })); }
        if used_p >= max_p { return answer(429, json!({ "error": format!("{where_} / {pfull}: {used_p} of {max_p} sessions working on it. Wait for one, stop one, or pick another AI."), "limit": { "host": host, "used": used_p, "max": max_p, "provider": pfull } })); }
      } }
    let source = if kind == "host" { "hub".to_string() } else if kind == "host-claude" { s(&prov, "id") } else { String::new() };
    // a Claude session lives where it ran (relayed runs run here, on this hub); a Jcode PGM host's, on that host
    let wher = if (kind == "host-claude" && s(&prov, "remoteKind") == "claude-cli-work") || hjcode { host_id.clone() } else { "hub".to_string() };
    // a session is resumed only where it ran and by the harness that made it (a Jcode PGM session id means nothing to Claude Code)
    let resume = !fresh && !s(&st, "claudeSessionId").is_empty() && { let w0 = s(&st, "claudeSessionWhere"); (if w0.is_empty() { "hub".to_string() } else { w0 }) == wher }
        && { let h0 = s(&st, "sessionHarness"); (if h0.is_empty() { crate::jcode::CLAUDE_HARNESS.to_string() } else { h0 }) == harness };
    { let wh = wher.clone(); sessions::supdate(w, a, &sx, |m| { m.insert("claudeSessionWhere".into(), json!(wh)); m.insert("sessionHarness".into(), json!(harness)); if !resume { m.remove("claudeSessionId"); } }); }
    // attached files (the marker block at the end of the message): Claude gets their paths and may read that folder
    let wm = hub.work_message(a, &message, json!({ "project": wd.project }));
    let add_dirs: Vec<String> = wm["dirs"].as_array().cloned().unwrap_or_default().iter().filter_map(|d| d.as_str().map(String::from)).collect();
    let text = wm["text"].as_str().unwrap_or(&message).to_string();
    let mut input = if context.is_empty() { text } else { format!("{text}\n\n---\n{context}") };
    { let note = w.fresh_note(a, &wd); if !note.is_empty() { input.push_str("\n\n"); input.push_str(&note); } }
    if !source.is_empty() { ai.usage.note_activity(&source); }
    let (tx, rx) = mpsc::unbounded_channel();
    let run_id = pgm_core_internal::crypto::random_hex(8);
    w.runs.lock().unwrap_or_else(|e| e.into_inner()).insert(sx.key.clone(), Run { id: run_id.clone(), tx: tx.clone(), pid: None, remote: String::new(), pending: HashMap::new(), ev: RunLog::default(), last: now_ms(), answered: false, prov: s(&prov, "id"), project: wd.project.clone(), branch: wd.branch.clone(), started: now_ms() });
    // its id first, so the page can join it again after a reload (/api/work/attach); older pages ignore it
    w.emit(&sx.key, json!({ "type": "bridge_run", "id": run_id, "session": sx.sid }));
    w.busy_changed.notify_one();
    // the page hears something every 15 s while Claude thinks
    // and every 15 s the run is checked: a run that sent nothing of its own for stallAfter (12 minutes: longer than the
    // longest quiet tool call) is ended as silent, so its session counts as idle and frees its slot (plans/multi-session.md § 3)
    { let (h, u, id, sid, a2) = (hub.clone(), sx.key.clone(), run_id.clone(), sx.sid.clone(), a.clone()); let stall = w.stall_after(); tokio::spawn(async move { loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        let last = h.work().runs.lock().unwrap_or_else(|e| e.into_inner()).get(&u).filter(|r| r.id == id).map(|r| r.last);
        let Some(last) = last else { return };
        if now_ms().saturating_sub(last) > stall {
            h.work().emit(&u, json!({ "type": "bridge_error", "text": format!("The run sent nothing for {} minutes, so it was ended (stalled). Send again to go on.", stall / 60_000) }));
            sessions::touch(&a2, &sid, |m| { m.insert("ended".into(), json!({ "why": "silent", "at": now_ms() })); });
            h.work().stop(&*h, &u); return;
        }
        h.work().emit(&u, json!({ "type": "ping" }));
    } }); }
    let mut relay_env: Vec<(String, String)> = vec![];
    if kind == "host-claude" && s(&prov, "remoteKind") != "claude-cli-work" {
        // a host's Claude login, shared as AI only: Claude Code runs HERE, only its model calls travel to the host
        let tok = w.llm_token(&a.username, &host_id, &prov_id);
        relay_env = vec![("ANTHROPIC_BASE_URL".into(), format!("http://127.0.0.1:{}/llm/{tok}", hub.cfg().port)), ("ANTHROPIC_AUTH_TOKEN".into(), "viberoller-relay".into()), ("ANTHROPIC_API_KEY".into(), String::new()), ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into())];
        w.emit(&sx.key, json!({ "type": "bridge_info", "text": format!("Using {nick}{}; the work happens here on the server.", if owner.is_empty() { String::new() } else { format!(" ({}'s AI)", hub.name_of(&owner)) }) }));
    } else if kind == "host-claude" || hjcode {
        // legacy: Claude Code runs on the host (only for a host that allows work); or Jcode PGM there (hjcode, the host
        // announced work-jcode: `harness` and `stateModel` are the optional fields it reads)
        let mut payload = json!({ "account": a.username, "display": a.display, "project": wd.project, "hub": wd.hub, "branch": wd.branch, "repoUrl": w.repo_url_for(&wd), "message": input, "systemPrompt": w.system_prompt(a, &wd), "model": model,
            "resume": if resume { s(&st, "claudeSessionId") } else { String::new() }, "permissionMode": hub.cfg().get("permissionMode"), "allowedTools": hub.cfg().get("allowedTools"), "hubUrl": hub.public_url(), "env": Value::Object(w.work_env(&a.username).into_iter().map(|(k, v)| (k, json!(v))).collect()) });
        if hjcode { payload["harness"] = json!(harness); payload["stateModel"] = json!(s(&st, "model")); }
        match ai.run_work(&host_id, &prov_id, payload) {
            Err(e) => { w.runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&sx.key); w.busy_changed.notify_one(); let _ = tx.send(line(json!({ "type": "bridge_error", "text": e.msg }))); let _ = tx.send(line(json!({ "type": "bridge_done", "code": 1 }))); drop(tx); return ndjson(rx); }
            Ok((job, _token, mut events)) => {
                if let Some(r) = w.runs.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&sx.key) { r.remote = job; }
                w.emit(&sx.key, json!({ "type": "bridge_info", "text": format!("Running on {nick}{}{}.", if owner.is_empty() { " (AI host)".to_string() } else { format!(" ({}'s AI host)", hub.name_of(&owner)) }, if hjcode { ", through Jcode PGM" } else { "" }) }));
                let (h, a2, id, sx2) = (hub.clone(), a.clone(), run_id.clone(), sx.clone());
                tokio::spawn(async move {
                    let w = h.work();
                    while let Some(m) = events.recv().await {
                        match m {
                            crate::ai_hosts::WorkMsg::Event(ev) => w.on_event(&*h, &a2, &sx2, ev, &source),
                            crate::ai_hosts::WorkMsg::Done(c) => { w.finish(&a2, &sx2, &id, json!(c)); return; }
                            crate::ai_hosts::WorkMsg::Error(t) => { w.emit(&sx2.key, json!({ "type": "bridge_error", "text": t })); w.finish(&a2, &sx2, &id, json!(1)); return; }
                        }
                    }
                    w.finish(&a2, &sx2, &id, json!(1));
                });
                drop(tx);
                return ndjson(rx);
            }
        }
    }
    // ---- here: Claude Code (claudeCommand / claudeArgs only for the regression runner's fake Claude; else the real
    // `claude`, the message on stdin), or Jcode PGM (the provider's binary, `run --ndjson`, the message as the last
    // argument, a private jcode home with the hub's MCP servers and the system prompt as its overlay)
    let hkind = Work::harness_kind(&prov);
    let mut hx = crate::harness::harness_for(hkind, &prov, &s(&st, "model"));
    let mcp_json = if hkind == crate::harness::HarnessKind::Jcode { w.mcp_json(a, &sx) } else { Value::Null };
    let sid = s(&st, "claudeSessionId");
    let claude_command = hub.cfg().get("claudeCommand").and_then(Value::as_str).unwrap_or("claude").to_string();
    let claude_args: Vec<String> = hub.cfg().get("claudeArgs").and_then(Value::as_array).map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let state_model = s(&st, "model");
    let spec = hx.command(&crate::harness::RunContext { prov: &prov, input: &input, resume: if resume { Some(sid.as_str()) } else { None }, model: &model, state_model: &state_model, acct_dir: &a.dir, cwd: &wd.dir,
        system_prompt: &system_prompt, mcp_json: &mcp_json, mcp_config: &mcp_config, permission_mode: &permission_mode, allowed_tools: &tools, add_dirs: &add_dirs, claude_command: &claude_command, claude_args: &claude_args });
    if let Some(b) = hx.banner() { w.emit(&sx.key, json!({ "type": "bridge_info", "text": b })); }
    let (command, harness_env, input) = (spec.program.clone(), spec.env.clone(), spec.stdin.clone());
    let mut cmd = tokio::process::Command::new(&command);
    cmd.args(&spec.args).current_dir(&spec.cwd).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(false);
    let fe = w.work_env(&a.username); let get = |k: &str| fe.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let fj_env = crate::fj::env(&a.dir.join(".fj"), &get("FORGEJO_URL"), &get("FORGEJO_TOKEN"), &get("FORGEJO_LOGIN")); // fj signed in as them
    for (k, v) in prov_env.iter().chain(relay_env.iter()).chain(harness_env.iter()).chain(fe.iter()).chain(fj_env.iter()) { cmd.env(k, v); }
    #[cfg(windows)]
    { cmd.creation_flags(0x0800_0000); }
    let (h, a2, id, sx2) = (hub.clone(), a.clone(), run_id.clone(), sx.clone());
    drop(tx);
    tokio::spawn(async move {
        let w = h.work();
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => { w.emit(&sx2.key, json!({ "type": "bridge_error", "text": format!("Could not start {command}: {e}") })); w.finish(&a2, &sx2, &id, json!(-2)); return; }
        };
        if let Some(r) = w.runs.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&sx2.key) { r.pid = child.id(); }
        if let Some(mut stdin) = child.stdin.take() { let inp = input.clone(); tokio::spawn(async move { let _ = stdin.write_all(inp.as_bytes()).await; let _ = stdin.shutdown().await; }); }
        let err_task = child.stderr.take().map(|e| { let (h, u) = (h.clone(), sx2.key.clone()); tokio::spawn(async move { use tokio::io::AsyncReadExt; let mut e = e; let mut buf = vec![0u8; 8192]; while let Ok(n) = e.read(&mut buf).await { if n == 0 { break; } h.work().emit(&u, json!({ "type": "stderr", "text": String::from_utf8_lossy(&buf[..n]) })); } }) });
        if let Some(out) = child.stdout.take() {
            let mut lines = tokio::io::BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                for e in hx.map_line(&l) {
                    match e { crate::harness::HubEvent::Raw(_) => w.emit(&sx2.key, e.into_value()), e => w.on_event(&*h, &a2, &sx2, e.into_value(), &source) }
                }
            }
        }
        let code = child.wait().await.ok().and_then(|s| s.code()).map(|c| json!(c)).unwrap_or(Value::Null);
        if let Some(t) = err_task { let _ = t.await; }
        w.finish(&a2, &sx2, &id, code);
    });
    ndjson(rx)
}

/// The MCP tools of a run (here: /internal/<tool> with this hub's secret; on a host: /node/job/<id>/internal/<tool>).
pub async fn tool(hub: &Arc<dyn AiHub>, which: &str, account: &str, b: &Value) -> Value {
    let (name, args) = (s(b, "name"), if b["args"].is_object() { b["args"].clone() } else { json!({}) });
    if which == "board" {
        let Some(op) = pgm_collab_internal::board::tool_op(&name) else { return json!({ "ok": false, "error": format!("Unknown board tool: {name}") }) };
        if hub.account(account).is_none() { return json!({ "ok": false, "error": "Unknown account." }); }
        return hub.board_tool(op, account, &args);
    }
    let Some(a) = hub.account(account) else { return json!({ "ok": false, "error": "Unknown account." }) };
    let w = hub.work();
    if which == "team" {
        if name.starts_with("notes_") { return hub.notes_tool(&name, &args, &hub.name_of(account)); }
        match name.as_str() {
            "ask_user" => return w.ask(&a, b, &args).await, // T-245: a question series for the person of the run
            "sessions_search" => {
                let q: String = args["q"].as_str().or(args["query"].as_str()).unwrap_or("").chars().take(200).collect();
                let me = hub.name_of(account);
                let items: Vec<Value> = archive_search(&a, &hub.mills_dir(), &q).into_iter().take(20).map(|it| json!({ "id": it["id"], "title": it["title"], "savedAt": it["savedAt"], "entries": it["count"], "hits": it["hits"],
                    "snippets": it["snippets"].as_array().map(|x| x.iter().map(|sn| json!(format!("{}: {}", if sn["cls"] == json!("me") { me.clone() } else { "Claude".into() }, s(sn, "text")))).collect::<Vec<_>>()).unwrap_or_default() })).collect();
                return json!({ "ok": true, "result": { "query": q, "items": items } });
            }
            "my_providers" => {
                let st = w.state(&a);
                let list: Vec<Value> = providers_list(&**hub, account).into_iter().map(|p| json!({ "id": p["id"], "label": p["label"], "kind": p["kind"], "owner": if s(&p, "owner").is_empty() { json!("host") } else { p["owner"].clone() }, "shared": p["shared"], "models": p["models"] })).collect();
                return json!({ "ok": true, "result": { "providers": list, "current": if s(&st, "provider").is_empty() { "host".into() } else { s(&st, "provider") }, "model": s(&st, "model") } });
            }
            _ => return hub.team_tool(&name, &args, &a), // the chat, presence, calls, servers
        }
    }
    // pc-tool: the person's own AI host with direct computer access first (the native app), else the browser's open folder
    if let Some(h) = hub.ai().pc_host_for(account) {
        match hub.ai().pc_tool(&h, &name, &args).await { Ok(v) => return v, Err(e) => if !w.busy(account) { return json!({ "ok": false, "error": format!("{}'s computer did not answer: {}", a.display, e.msg) }) } }
    }
    let id = pgm_core_internal::crypto::random_hex(8);
    let (otx, orx) = oneshot::channel();
    // the session that asked (its MCP relay names it, 5.15.2), else the person's run that is going
    let key = sessions::resolve(&a, b["session"].as_str().unwrap_or("")).filter(|sx| !s(b, "session").is_empty() && w.busy_key(&sx.key)).map(|sx| sx.key).or_else(|| w.run_key_of(account)).unwrap_or_else(|| account.to_string());
    { let mut runs = w.runs.lock().unwrap_or_else(|e| e.into_inner()); match runs.get_mut(&key) { Some(r) => { r.pending.insert(id.clone(), otx); } None => return json!({ "ok": false, "error": format!("{}'s browser is not connected (and no AI host of theirs offers their computer).", a.display) }) } }
    w.emit(&key, json!({ "type": "pc_tool", "id": id, "name": name, "args": args }));
    match tokio::time::timeout(Duration::from_secs(120), orx).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => json!({ "ok": false, "error": "Run ended" }),
        Err(_) => { if let Some(r) = w.runs.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&key) { r.pending.remove(&id); } json!({ "ok": false, "error": "Timed out waiting for the browser." }) }
    }
}
/// Everything a person may pick (bridge/providers.js list): this hub's Claude, the own / shared ones, the hosts'.
pub fn providers_list(hub: &dyn AiHub, user: &str) -> Vec<Value> {
    let w = hub.work();
    let mut out = vec![]; if w.hub_claude() { out.push(w.host_entry()); }
    for p in w.own_providers() { if p["shared"] == json!(true) || p["owner"] == json!(user) {
        let mut v = json!({ "id": p["id"], "kind": p["kind"], "label": p["label"], "owner": p["owner"], "shared": p["shared"] == json!(true), "models": if p["models"].is_array() { p["models"].clone() } else { json!([]) }, "hasKey": !s(&p, "apiKey").is_empty(), "createdAt": p["createdAt"] });
        for k in ["baseUrl", "configDir"] { if !s(&p, k).is_empty() { v[k] = p[k].clone(); } }
        out.push(v);
    } }
    out.extend(hub.ai().providers_for(user));
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
/// Saves a conversation into Past sessions: the entry `id` (a new one when "" or not an id) with its title, its harness
/// session and its transcript; returns its id (a session being archived, T-284).
pub(crate) fn archive_put(a: &Account, mills: &Path, root: &Path, id: &str, title: &str, sid: &str, tr: &[Value]) -> String {
    let dir = archive_dir(a, mills, root);
    let id = if archive_file(&dir, id).is_some() { id.to_string() } else { pgm_core_internal::crypto::random_hex(8) };
    let Some(f) = archive_file(&dir, &id) else { return String::new() };
    let prev = read_json(&f).unwrap_or(json!({}));
    let _ = std::fs::create_dir_all(&dir);
    let title = clean_title(if title.is_empty() { prev["title"].as_str().unwrap_or("") } else { title });
    write_json(&f, &json!({ "id": id, "title": title, "savedAt": iso(), "sessionId": sid, "transcript": tr.iter().filter(|t| t["cls"].is_string() && t["text"].is_string()).cloned().collect::<Vec<_>>() }));
    id
}

// ---------- shared Work sessions (watch-only) ----------
impl Work {
    /// Is this session of the owner's the one they share?
    fn shares_session(&self, owner: &str, sid: &str) -> bool { self.shares.lock().unwrap_or_else(|e| e.into_inner()).get(owner).map(|s| s.sid == sid).unwrap_or(false) }
    fn share_line(&self, owner: &str, t: &str, text: &str) {
        if text.is_empty() { return; }
        let mut sh = self.shares.lock().unwrap_or_else(|e| e.into_inner()); let Some(s) = sh.get_mut(owner) else { return };
        s.n += 1; s.log.push(json!({ "n": s.n, "t": t, "text": text.chars().take(20000).collect::<String>(), "ts": iso() }));
        if s.log.len() > 400 { let k = s.log.len() - 400; s.log.drain(0..k); }
        s.woke.notify_waiters();
    }
    fn share_event(&self, owner: &str, ev: &Value) {
        if !self.shares.lock().unwrap_or_else(|e| e.into_inner()).contains_key(owner) { return; }
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
    pub fn shares_for(&self, user: &str) -> Vec<Value> { self.shares.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(o, _)| *o != user).map(|(o, s)| json!({ "user": o, "conv": s.conv, "since": s.since })).collect() }
    pub fn shares_all(&self) -> Vec<Value> { self.shares.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(o, s)| json!({ "user": o, "conv": s.conv, "since": s.since })).collect() }
    pub fn share_of(&self, owner: &str) -> String { self.shares.lock().unwrap_or_else(|e| e.into_inner()).get(owner).map(|s| s.conv.clone()).unwrap_or_default() }
    pub fn share_stop(&self, owner: &str) { let gone = self.shares.lock().unwrap_or_else(|e| e.into_inner()).remove(owner); if let Some(s) = gone { s.woke.notify_waiters(); self.shares_changed.notify_one(); } }
}

const NO_FORGEJO_TOKEN: &str = "Save your own Forgejo token first (Settings → My Forgejo): the repos you can see, and a new private repo, are asked of Forgejo as you.";
/// worktree.js repos' lines → [{ name, private, fork, mirror, url, description }].
fn parse_repos(out: &str) -> Vec<Value> {
    let re = regex::Regex::new(r"^(\S+/\S+?)((?: \((?:private|fork|mirror)\))*)  (\S+)\s*$").unwrap();
    let mut v: Vec<Value> = vec![];
    for line in out.lines() {
        if let Some(c) = re.captures(line) {
            let tags = &c[2];
            v.push(json!({ "name": &c[1], "private": tags.contains("(private)"), "fork": tags.contains("(fork)"), "mirror": tags.contains("(mirror)"), "url": &c[3], "description": "" }));
        } else if line.starts_with("    ") { if let Some(last) = v.last_mut() { last["description"] = json!(line.trim()); } }
    }
    v
}

/// The Work routes (signed in as `a`); None = not one of them.
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    let w = hub.work(); let u = &a.username;
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let p = r.path.as_str();
    if let Some(resp) = sessions::route(hub, r, a).await { return Some(resp); }
    if let Some(resp) = crate::gitpanel::route(hub, r, a).await { return Some(resp); }
    // the session a request names (plans/multi-session.md; none = the first one, as older pages send it)
    let sess = |b: &Value| sessions::resolve(a, &sessions::wanted(r, b));
    let no_session = || answer(404, json!({ "error": "No such session." }));
    Some(match p {
        "/api/chat" if post => chat(hub, a, r).await,
        "/api/stop" if post => { let Some(sx) = sess(&r.json()) else { return Some(no_session()) }; w.stop(&**hub, &sx.key); answer(200, json!({ "ok": true })) }
        "/api/pc-result" if post => {
            let b = r.json(); let pre = format!("{u}#");
            let entry = w.runs.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().filter(|(k, _)| *k == u || k.starts_with(&pre)).find_map(|(_, run)| run.pending.remove(&s(&b, "id")));
            if let Some(tx) = entry { let mut v = json!({ "ok": b["ok"].as_bool().unwrap_or(false) || (!b["ok"].is_null() && b["ok"] != json!(false) && b["ok"] != json!(0) && b["ok"] != json!("")) }); for k in ["result", "error"] { if let Some(x) = b.get(k) { v[k] = x.clone(); } } let _ = tx.send(v); }
            answer(200, json!({ "ok": true }))
        }
        // the AI's question (T-245): the page answers it, or says the person is busy with it (the timeout starts over)
        "/api/work/answer" if post => { let (st, v) = w.answer(a, &r.json()); answer(st, v) }
        "/api/work/questions" if get => answer(200, json!({ "questions": w.open_questions(a, &r.param("session").unwrap_or_default()) })),
        "/api/usage" if get => answer(200, w.usage_status(&**hub, a)),
        "/api/model" if post => {
            let b = r.json(); let Some(sx) = sess(&b) else { return Some(no_session()) }; let st = sessions::sstate(w, a, &sx); let ai = hub.ai();
            let want = if b.get("provider").is_none() { s(&st, "provider") } else { s(&b, "provider") };
            let prov = if !want.is_empty() && want != "host" { w.provider_get(ai, &want, u) } else { w.default_for(ai, u).or_else(|| w.provider_get(ai, "host", "")) };
            let Some(prov) = prov else { return Some(answer(400, json!({ "error": "Unknown provider." }))) };
            if Work::harness_of(&prov).is_none() { return Some(answer(400, json!({ "error": "That provider cannot run Work: it is neither Claude Code nor Jcode PGM. Use it for the API or the Unity section instead." }))); }
            let ids: Vec<String> = if prov["kind"] == json!("host") { w.model_ids() } else { prov["models"].as_array().map(|m| m.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
            let Some(model) = b["model"].as_str().map(String::from) else { return Some(answer(400, json!({ "error": "Unknown model." }))) };
            // a jcode provider without a model list takes any model id (jcode resolves it; empty = jcode's default)
            let open = prov["kind"] == json!(crate::jcode::KIND) && ids.is_empty();
            if !model.is_empty() && !open && !ids.contains(&model) { return Some(answer(400, json!({ "error": "Unknown model." }))); }
            let pid = s(&prov, "id");
            let st = sessions::supdate(w, a, &sx, |m| {
                if model.is_empty() { m.remove("model"); } else { m.insert("model".into(), json!(model)); }
                if want.is_empty() || want == "host" { m.remove("provider"); } else { m.insert("provider".into(), json!(pid)); }
                if m.contains_key("provider") && model.is_empty() { if let Some(f) = ids.first() { m.insert("model".into(), json!(f)); } }
            });
            let eff = w.effective(ai, u, &s(&st, "provider")).map(|p| s(&p, "id")).unwrap_or_default();
            let effective = if s(&st, "model").is_empty() { w.default_model_of(a) } else { s(&st, "model") };
            answer(200, json!({ "model": s(&st, "model"), "provider": if s(&st, "provider").is_empty() { "host".into() } else { s(&st, "provider") }, "effective": effective, "effectiveProvider": eff }))
        }
        "/api/projects" if get => answer(200, json!({ "projects": w.project_list(), "project": w.project_of(a) })),
        // { name } = start a new project; { url, name? } = pull in a repo (bridge/worktree.js new / clone, as this person)
        "/api/projects" if post => {
            let b = r.json(); let (name, url) = (s(&b, "name"), s(&b, "url"));
            let mut env = w.work_env(u); env.push(("USERNAME".into(), u.clone())); env.push(("USER".into(), u.clone()));
            let args: Vec<&str> = if url.is_empty() { vec!["new", name.as_str()] } else if name.is_empty() { vec!["clone", url.as_str()] } else { vec!["clone", url.as_str(), name.as_str()] };
            let wt = &w.wt; let (ok, out, err) = tokio::task::block_in_place(|| wt.tool(&args, &env));
            let made = regex::Regex::new(r"(?m)^(?:Created project|Pulled in) ([^:]+):").unwrap().captures(&out).map(|c| c[1].to_string());
            match made.filter(|_| ok) {
                Some(pj) => { w.projects_changed(); answer(200, json!({ "project": pj, "projects": w.project_list() })) }
                None => { let e = err.trim().lines().last().unwrap_or("").trim_start_matches("Error: ").to_string(); let e = if e.is_empty() { out.trim().lines().last().unwrap_or("Could not make the project.").to_string() } else { e }; answer(if e.contains("exists already") { 409 } else { 400 }, json!({ "error": e })) }
            }
        }
        // the Forgejo repos this person can see, to clone from (T-238): worktree.js repos, as them; it needs their own token
        "/api/projects/repos" if get => {
            let env = w.work_env(u);
            if env.is_empty() { return Some(answer(400, json!({ "error": NO_FORGEJO_TOKEN, "needsToken": true }))); }
            let q: String = r.param("q").unwrap_or_default().chars().filter(|c| !c.is_control()).take(80).collect::<String>().split_whitespace().map(|x| x.trim_start_matches('-')).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
            let mut args: Vec<&str> = vec!["repos"]; args.extend(q.split(' ').filter(|x| !x.is_empty()));
            let wt = &w.wt; let (ok, out, err) = tokio::task::block_in_place(|| wt.tool(&args, &env));
            if !ok { let e = err.trim().lines().last().unwrap_or("Forgejo did not answer.").to_string(); return Some(answer(502, json!({ "error": e }))); }
            answer(200, json!({ "repos": parse_repos(&out), "q": q }))
        }
        // a new empty project, private on Forgejo (T-238), in this order: the checks (name, taken, token), the repo on
        // Forgejo (repo-create, as them), the local project (new), and publish (origin = Forgejo, every branch pushed)
        "/api/projects/create" if post => {
            let b = r.json(); let name = s(&b, "name").trim().to_string();
            if !pgm_collab_internal::worktree::Worktrees::valid_name(&name) { return Some(answer(400, json!({ "error": "A project name is 1-64 characters: letters, digits, dots, dashes and underscores, starting with a letter or digit." }))); }
            if w.wt.name_taken(&name) { return Some(answer(409, json!({ "error": format!("A project named \"{name}\" exists already.") }))); }
            let mut env = w.work_env(u);
            let Some(login) = w.forgejo_login(u).filter(|_| !env.is_empty()) else { return Some(answer(400, json!({ "error": NO_FORGEJO_TOKEN, "needsToken": true }))) };
            let owner = { let o = s(&b, "owner"); if o.is_empty() { login } else { o } };
            if !owner.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) || owner.is_empty() || owner.len() > 100 { return Some(answer(400, json!({ "error": "That owner name is not valid." }))); }
            let full = format!("{owner}/{name}");
            let desc: String = s(&b, "description").chars().filter(|c| !c.is_control()).take(200).collect();
            let wt = &w.wt;
            let fail = |out: &str, err: &str, what: &str| -> (u16, String) { let e = err.trim().lines().last().unwrap_or("").trim_start_matches("Error: ").to_string(); let e = if e.is_empty() { out.trim().lines().last().unwrap_or(what).to_string() } else { e }; (if e.contains("409") || e.contains("already exists") || e.contains("exists already") { 409 } else { 502 }, e) };
            let mut rc: Vec<&str> = vec!["repo-create", full.as_str(), "--private"]; if !desc.is_empty() { rc.push("--description"); rc.push(desc.as_str()); }
            let (ok, out, err) = tokio::task::block_in_place(|| wt.tool(&rc, &env));
            if !ok { let (st, e) = fail(&out, &err, "Forgejo did not make the repo."); return Some(answer(st, json!({ "error": e, "stage": "repo-create" }))); }
            env.push(("USERNAME".into(), u.clone())); env.push(("USER".into(), u.clone()));
            let (ok, out, err) = tokio::task::block_in_place(|| wt.tool(&["new", name.as_str()], &env));
            if !ok { let (st, e) = fail(&out, &err, "Could not make the project."); return Some(answer(st, json!({ "error": format!("{e} ({full} was made on Forgejo; it is empty and yours to delete or reuse.)"), "stage": "new", "repo": full }))); }
            let (ok, out, err) = tokio::task::block_in_place(|| wt.tool(&["--project", name.as_str(), "publish", full.as_str(), "--private"], &env));
            w.projects_changed();
            if !ok { let (st, e) = fail(&out, &err, "Could not publish the project."); return Some(answer(st, json!({ "error": format!("{e} (the project {name} exists here; \"publish\" can be run again.)"), "stage": "publish", "project": name, "repo": full }))); }
            answer(200, json!({ "project": name, "repo": full, "private": true, "log": out.trim(), "projects": w.project_list() }))
        }
        "/api/workspace" if post => {
            let b = r.json();
            // no project: the AI only, in the person's scratch folder (T-238)
            if b["none"] == json!(true) || s(&b, "project") == NO_PROJECT {
                let Some(sx) = sess(&b) else { return Some(no_session()) };
                if w.busy_key(&sx.key) { return Some(answer(409, json!({ "error": "Wait for the current reply (or stop it) before switching." }))); }
                let wd = match w.wt.scratch(u) { Ok(x) => x, Err(e) => return Some(answer(500, json!({ "error": e }))) };
                let switched = !w.is_none(a, &sx);
                sessions::supdate(w, a, &sx, |m| { if switched { m.remove("claudeSessionId"); } m.remove("project"); m.insert("noProject".into(), json!(true)); });
                return Some(answer(200, json!({ "project": NO_PROJECT, "none": true, "dir": wd.dir.display().to_string(), "branch": "" })));
            }
            let want = if !s(&b, "project").is_empty() { s(&b, "project") } else if !b["workspace"].is_null() && b["workspace"] != json!("") { HUB_PROJECT.into() } else { String::new() };
            let pj = match w.wt.project(&want) { Ok(p) => p, Err(e) => return Some(answer(400, json!({ "error": e }))) };
            let Some(sx) = sess(&b) else { return Some(no_session()) };
            if w.busy_key(&sx.key) { return Some(answer(409, json!({ "error": "Wait for the current reply (or stop it) before switching." }))); }
            let wd = match if sx.primary { w.wt.ensure(u, &pj.name) } else { w.wt.ensure_session(u, sx.hex(), &pj.name) } { Ok(x) => x, Err(e) => return Some(answer(500, json!({ "error": format!("Could not prepare your worktree: {e}") }))) };
            let switched = w.project_in(a, &sx) != pj.name;
            sessions::supdate(w, a, &sx, |m| { m.remove("noProject"); if switched { m.remove("claudeSessionId"); } if pj.hub { m.remove("project"); } else { m.insert("project".into(), json!(pj.name)); } });
            w.check_fresh(a);
            answer(200, json!({ "project": pj.name, "dir": wd.dir.display().to_string(), "branch": wd.branch }))
        }
        // ----- the Work conversation + queue, kept across restarts -----
        "/api/work/current" if get => { let Some(sx) = sess(&json!({})) else { return Some(no_session()) }; answer(200, sessions::scurrent(a, &sx)) }
        "/api/work/attach" if get => { let Some(sx) = sess(&json!({})) else { return Some(no_session()) }; answer(200, w.attach(&sx.key, &r.param("run").unwrap_or_default(), r.param("after").and_then(|x| x.parse().ok()).unwrap_or(0)).await) }
        "/api/work/current" if post => {
            let b = r.json(); let Some(sx) = sess(&b) else { return Some(no_session()) }; let mut st = sessions::scurrent(a, &sx);
            let Some(list) = b["transcript"].as_array() else { return Some(answer(400, json!({ "error": "transcript must be a list." }))) };
            let mut out: Vec<Value> = list.iter().skip(list.len().saturating_sub(3000)).filter(|t| ["me", "claude", "tool"].contains(&t["cls"].as_str().unwrap_or("")) && t["text"].is_string()).map(|t| json!({ "cls": t["cls"], "text": s(t, "text").chars().take(200 * 1024).collect::<String>() })).collect();
            let mut total = 0usize; let mut keep = 0usize;
            for (i, t) in out.iter().enumerate().rev() { total += t["text"].as_str().map(|x| x.encode_utf16().count()).unwrap_or(0); if total > 6 * 1024 * 1024 { keep = i + 1; break; } }
            if keep > 0 { out.drain(0..keep); }
            let n = out.len();
            st["transcript"] = json!(out);
            st["archiveId"] = b["archiveId"].as_str().filter(|x| !x.is_empty()).map(|x| json!(x.chars().take(80).collect::<String>())).unwrap_or(Value::Null);
            st["archiveTitle"] = b["archiveTitle"].as_str().filter(|x| !x.is_empty()).map(|x| json!(x.chars().take(200).collect::<String>())).unwrap_or(Value::Null);
            // how far into a running reply this transcript goes (the page joins the run from there after a reload)
            st["run"] = match (b["run"]["id"].as_str(), b["run"]["n"].as_u64()) { (Some(id), Some(n)) if id.len() <= 32 && id.bytes().all(|c| c.is_ascii_hexdigit()) => json!({ "id": id, "n": n }), _ => Value::Null };
            sessions::save_scurrent(a, &sx, &mut st);
            answer(200, json!({ "ok": true, "items": n }))
        }
        "/api/work/queue" if post => {
            let b = r.json(); let Some(sx) = sess(&b) else { return Some(no_session()) }; let mut st = sessions::scurrent(a, &sx);
            let Some(q) = b["queue"].as_array() else { return Some(answer(400, json!({ "error": "queue must be a list." }))) };
            let clean = |m: &Value| m.as_str().filter(|x| !x.trim().is_empty()).map(|x| json!(x.chars().take(50 * 1024).collect::<String>()));
            st["queue"] = json!(q.iter().filter_map(clean).take(50).collect::<Vec<_>>());
            st["sendNow"] = clean(&b["sendNow"]).unwrap_or(Value::Null);
            let n = st["queue"].as_array().map(|x| x.len()).unwrap_or(0);
            sessions::save_scurrent(a, &sx, &mut st);
            answer(200, json!({ "ok": true, "queue": n }))
        }
        // ----- shared Work sessions -----
        "/api/work/share" if post => {
            let b = r.json(); let conv = s(&b, "conv");
            if conv.is_empty() { w.share_stop(u); return Some(answer(200, json!({ "share": "" }))); }
            // which session it shows (none named = the first one); sharing another one moves the share
            let Some(sx) = sess(&b) else { return Some(no_session()) };
            if conv != "room" { return Some(answer(400, json!({ "error": "A Work session can only be shared with everyone." }))); }
            let since = iso();
            let kinds = |c: &str| match c { "me" => Some("user"), "claude" => Some("claude"), "tool" => Some("tool"), "sys" => Some("sys"), _ => None };
            let mut log = vec![]; let mut n = 0;
            let tr = b["transcript"].as_array().cloned().unwrap_or_default();
            for t in tr.iter().skip(tr.len().saturating_sub(150)) { if let (Some(k), Some(text)) = (kinds(t["cls"].as_str().unwrap_or("")), t["text"].as_str()) { n += 1; log.push(json!({ "n": n, "t": k, "text": text.chars().take(20000).collect::<String>(), "ts": since })); } }
            { let mut sh = w.shares.lock().unwrap_or_else(|e| e.into_inner()); let prev = sh.remove(u); let live = prev.as_ref().map(|p| p.live.clone()).unwrap_or(json!({ "typing": "", "queue": [] }));
              if let Some(p) = &prev { p.woke.notify_waiters(); }
              sh.insert(u.clone(), Share { conv: conv.clone(), since, n, log, live, ver: 0, woke: prev.map(|p| p.woke).unwrap_or_default(), sid: sx.sid.clone(), key: sx.key.clone() }); }
            w.shares_changed.notify_one();
            answer(200, json!({ "share": conv, "session": sx.sid }))
        }
        "/api/work/live" if post => {
            let b = r.json(); let mut sh = w.shares.lock().unwrap_or_else(|e| e.into_inner());
            let Some(sv) = sh.get_mut(u) else { return Some(answer(200, json!({ "ok": false }))) };
            sv.live = json!({ "typing": b["typing"].as_str().map(|t| t.chars().take(4000).collect::<String>()).unwrap_or_default(), "queue": b["queue"].as_array().map(|q| q.iter().take(20).map(|x| json!(match x { Value::String(t) => t.chars().take(2000).collect::<String>(), v => v.to_string() })).collect::<Vec<_>>()).unwrap_or_default() });
            sv.ver += 1; sv.woke.notify_waiters();
            answer(200, json!({ "ok": true }))
        }
        "/api/work/watch" if get => {
            let owner = r.param("user").unwrap_or_default(); let after: u64 = r.param("after").and_then(|x| x.parse().ok()).unwrap_or(0);
            let ver: Option<u64> = r.param("ver").and_then(|x| x.parse().ok()); let since = r.param("since").unwrap_or_default();
            let view = || -> (Value, bool) {
                let sh = w.shares.lock().unwrap_or_else(|e| e.into_inner());
                let Some(sv) = sh.get(&owner).filter(|_| owner != *u) else { return (json!({ "stopped": true }), true) };
                let restart = since != sv.since;
                let ready = restart || sv.log.iter().any(|e| e["n"].as_u64().unwrap_or(0) > after) || ver != Some(sv.ver);
                (json!({ "since": sv.since, "conv": sv.conv, "busy": w.busy_key(&sv.key), "session": sv.sid, "live": sv.live, "ver": sv.ver, "events": sv.log.iter().filter(|e| restart || e["n"].as_u64().unwrap_or(0) > after).cloned().collect::<Vec<_>>(), "after": sv.n, "restart": restart }), ready)
            };
            let woke = w.shares.lock().unwrap_or_else(|e| e.into_inner()).get(&owner).map(|s| s.woke.clone());
            if let Some(woke) = woke {
                let n = woke.notified(); tokio::pin!(n); n.as_mut().enable();
                let (v, ready) = view(); if ready { return Some(answer(200, v)); }
                let _ = tokio::time::timeout(Duration::from_secs(25), n).await;
            }
            answer(200, view().0)
        }
        // ----- past sessions -----
        p if post && p.starts_with("/api/archive/") => {
            let dir = archive_dir(a, &hub.mills_dir(), hub.root()); let b = r.json();
            match p {
                "/api/archive/list" => answer(200, json!({ "items": archive_list(&dir) })),
                "/api/archive/search" => answer(200, json!({ "items": archive_search(a, &hub.mills_dir(), &s(&b, "q").chars().take(200).collect::<String>()) })),
                "/api/archive/save" => {
                    let Some(tr) = b["transcript"].as_array().filter(|t| !t.is_empty()) else { return Some(answer(400, json!({ "error": "Nothing to save yet." }))) };
                    let Some(sx) = sess(&b) else { return Some(no_session()) };
                    let sid = s(&sessions::sstate(w, a, &sx), "claudeSessionId");
                    if sid.is_empty() { return Some(answer(400, json!({ "error": "This conversation has no Claude session yet." }))); }
                    let id = if s(&b, "id").is_empty() { pgm_core_internal::crypto::random_hex(8) } else { s(&b, "id") };
                    let Some(f) = archive_file(&dir, &id) else { return Some(answer(400, json!({ "error": "Bad conversation id." }))) };
                    let prev = read_json(&f).unwrap_or(json!({}));
                    let _ = std::fs::create_dir_all(&dir);
                    let title = clean_title(if s(&b, "title").is_empty() { prev["title"].as_str().unwrap_or("") } else { b["title"].as_str().unwrap_or("") });
                    let entry = json!({ "id": id, "title": title, "savedAt": iso(), "sessionId": sid, "transcript": tr.iter().filter(|t| t["cls"].is_string() && t["text"].is_string()).cloned().collect::<Vec<_>>() });
                    write_json(&f, &entry);
                    answer(200, json!({ "id": id, "title": title }))
                }
                "/api/archive/open" => {
                    let Some(sx) = sess(&b) else { return Some(no_session()) };
                    if w.busy_key(&sx.key) { return Some(answer(409, json!({ "error": "Claude is still working on your current conversation (Work shows it live). Wait for it, or stop it there, then open this one." }))); }
                    let Some(f) = archive_file(&dir, &s(&b, "id")) else { return Some(answer(400, json!({ "error": "Bad conversation id." }))) };
                    let Some(entry) = read_json(&f) else { return Some(answer(404, json!({ "error": "Conversation not found." }))) };
                    let sid = entry["sessionId"].clone(); sessions::supdate(w, a, &sx, |m| { m.insert("claudeSessionId".into(), sid); });
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
pub(crate) fn work_current(a: &Account) -> Value { current_of(&read_json(&work_current_file(a)).unwrap_or(json!({}))) }
/// A conversation + queue as the page reads it, whatever was stored.
pub(crate) fn current_of(s0: &Value) -> Value {
    let or_null = |k: &str| if s0[k].is_null() || s0[k] == json!("") { Value::Null } else { s0[k].clone() };
    json!({ "transcript": if s0["transcript"].is_array() { s0["transcript"].clone() } else { json!([]) }, "archiveId": or_null("archiveId"), "archiveTitle": or_null("archiveTitle"), "run": if s0["run"].is_object() { s0["run"].clone() } else { Value::Null }, "queue": if s0["queue"].is_array() { s0["queue"].clone() } else { json!([]) }, "sendNow": or_null("sendNow"), "updatedAt": or_null("updatedAt") })
}
pub(crate) fn save_work_current(a: &Account, st: &mut Value) {
    st["updatedAt"] = json!(iso());
    let f = work_current_file(a); let tmp = f.with_extension("json.tmp");
    if std::fs::write(&tmp, st.to_string()).is_ok() { let _ = std::fs::rename(&tmp, &f); }
}

/// /llm/<token>/…: this hub's own Claude Code reaching a host's shared login (local only, per-run token).
pub async fn llm(hub: &Arc<dyn AiHub>, r: &Req) -> Resp {
    let rest = &r.path["/llm/".len()..];
    let (tok, sub) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let behind = hub.cfg().get("behindProxy") == Some(&json!(true));
    let outside = r.header("cf-connecting-ip").is_some() || (behind && (r.header("x-forwarded-for").is_some() || r.header("x-real-ip").is_some()));
    let ent = if tok.len() == 48 && tok.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) && !sub.is_empty() { hub.work().llm.lock().unwrap_or_else(|e| e.into_inner()).get(tok).cloned() } else { None };
    let Some((_, host, prov, exp)) = ent.filter(|e| e.3 > now_ms() && r.peer.ip().is_loopback() && !outside) else { return answer(401, json!({ "type": "error", "error": { "type": "authentication_error", "message": "Unknown relay." } })) };
    let _ = exp;
    let path = if r.query.is_empty() { sub.to_string() } else { format!("{sub}?{}", r.query) };
    let headers: Vec<(String, String)> = r.headers.iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
    match hub.ai().login_fetch(&host, &prov, &path, r.method.as_str(), headers, r.body.to_vec()).await {
        Ok(rel) => {
            if rel.headers.iter().any(|(k, _)| k.starts_with("anthropic-ratelimit-unified-")) { hub.push_ai_state().await; }
            let mut b = hyper::Response::builder().status(rel.status);
            for (k, v) in &rel.headers { b = b.header(k.as_str(), v.as_str()); }
            b = b.header("cache-control", "no-store").header("x-accel-buffering", "no");
            b.body(crate::hub::stream_body(rel.body)).expect("response")
        }
        Err(e) => answer(if e.status == 0 { 503 } else { e.status }, json!({ "type": "error", "error": { "type": if e.status == 429 { "rate_limit_error" } else { "overloaded_error" }, "message": e.msg } })),
    }
}
/// The background freshness checks: every 5 minutes for anyone who ran something in the last day.
pub fn start(hub: &Arc<dyn AiHub>) {
    let h = hub.clone();
    tokio::spawn(async move { loop {
        tokio::time::sleep(Duration::from_secs(300)).await;
        for a in h.accounts() {
            let last = h.work().state(&a)["lastRunAt"].as_u64().unwrap_or(0); if last > 0 && now_ms() - last < 86_400_000 { h.work().check_fresh(&a); }
            // and the worktrees of their sessions used that day
            let Some(ix) = sessions::index_if_any(&a) else { continue };
            for (sid, m) in ix["sessions"].as_object().cloned().unwrap_or_default() {
                if sid == s(&ix, "primary") || !m["worktree"].is_object() || now_ms().saturating_sub(m["lastActivityAt"].as_u64().unwrap_or(0)) > 86_400_000 { continue; }
                h.work().check_fresh_in(&a, &s(&m["worktree"], "project"), Some(sid.trim_start_matches("s-")));
            }
        }
    } });
    // the shared sessions to the JavaScript hub (its room poll lists them; pages follow through it)
    let h = hub.clone();
    tokio::spawn(async move { loop {
        h.work().shares_changed.notified().await;
        let list = h.work().shares_all();
        h.chat_room_changed(); // the room's long poll lists them
        h.internal("/internal/front/shares", json!({ "shares": list }).to_string()).await;
    } });
    let h = hub.clone();
    tokio::spawn(async move { loop {
        h.work().busy_changed.notified().await;
        h.chat_room_changed(); // presence's "working" (plans/presence.md) reaches the pages
        let busy =h.work().any_busy() || h.api().busy() || h.unity().busy() || crate::unity_routes::room_busy(&*h);
        h.internal("/internal/front/work-busy", json!({ "busy": busy }).to_string()).await;
    } });
}
/// The prompt a run sends: the person's segments joined with a space, the Static laws after a blank line.
pub fn render_prompt(segs: &[Segment]) -> String {
    let own: Vec<&str> = segs.iter().filter(|s| s.class != Class::Static).map(|s| s.text.as_str()).collect();
    let laws: Vec<&str> = segs.iter().filter(|s| s.class == Class::Static && !s.text.is_empty()).map(|s| s.text.as_str()).collect();
    own.join(" ") + &if laws.is_empty() { String::new() } else { format!("\n\n{}", laws.join("\n\n")) }
}
/// Every volatile part (relative time, timestamp, random id, live counter) in the stable segments, by segment name.
pub fn lint_segments(segs: &[Segment]) -> Vec<LintHit> { segs.iter().flat_map(|s| lint(&s.id, s.class, &s.text)).collect() }

#[cfg(test)]
mod tests {
    use super::*;

    fn work_here() -> (Work, std::path::PathBuf) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge");
        let cfg = Config::load(&root);
        (Work::new(&root, &cfg), root)
    }
    fn wt(hub: bool, dir: &std::path::Path) -> Wt {
        Wt { dir: dir.to_path_buf(), branch: "work/sponge".into(), project: if hub { HUB_PROJECT.into() } else { "demo".into() }, repo: dir.join("_repo"), root: dir.to_path_buf(), main: "main".into(), hub }
    }

    /// The prompt's stable segments carry nothing volatile (no "N min ago", no timestamp, no random id, no counter) and
    /// the prompt is the same bytes when built again; the freshness result travels with the message, not here.
    #[test]
    fn the_work_system_prompt_segments_pass_the_cache_lint() {
        let (w, _) = work_here();
        let tmp = std::env::temp_dir().join(format!("vr-prompt-lint-{}", std::process::id())); let _ = std::fs::create_dir_all(&tmp);
        for hub in [true, false] {
            for admin in [true, false] {
                let a = Account { username: "sponge".into(), dir: tmp.clone(), admin, display: "Sponge".into() };
                let wd = wt(hub, &tmp);
                let segs = w.system_segments(&a, &wd);
                assert_eq!(segs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["1-intro", "2-where", "3-tools", "4-laws"]);
                assert!(segs.iter().all(|s| matches!(s.class, Class::Static | Class::Session)));
                assert!(!segs[3].text.is_empty(),"the hub's laws are in the prompt");
                let hits = lint_segments(&segs);
                assert!(hits.is_empty(), "the Work system prompt has volatile text: {:?}", hits.iter().map(|h| format!("{} / {}: {}", h.segment, h.pattern, h.snippet)).collect::<Vec<_>>());
                let p = render_prompt(&segs);
                assert_eq!(p, w.system_prompt(&a, &wd), "built twice: the same bytes");
                assert!(p.starts_with("You are talking with Sponge") && p.contains("OWN git worktree") && p.contains("\n\n"));
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
    #[test]
    fn the_lint_names_the_segment_when_volatile_text_creeps_in() {
        let segs = vec![Segment::new("2-where", Class::Session, "latest check (3 min ago): FRESH"), Segment::new("4-laws", Class::Static, "calm")];
        let hits = lint_segments(&segs);
        assert_eq!(hits.len(), 1); assert_eq!(hits[0].segment, "2-where");
        assert_eq!(render_prompt(&segs), "latest check (3 min ago): FRESH\n\ncalm");
    }
}
pub fn same_secret(a: &str, b: &str) -> bool { use subtle::ConstantTimeEq; bool::from(pgm_core_internal::crypto::sha256(a.as_bytes()).ct_eq(&pgm_core_internal::crypto::sha256(b.as_bytes()))) }
