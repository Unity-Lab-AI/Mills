//! AI hosts, hub side (moved from bridge/ai-nodes.js): machines that lend their AI to this hub (their Claude / ChatGPT
//! login, local models behind OpenAI-compatible endpoints, Unity's engines) by connecting OUT and long-polling for jobs.
//! The same registry (bridge/ai-hosts.json + config aiNodes), the same protocol (/node/hello, poll with the hub's
//! heartbeat, reply, stream, events, update), the same sessions file (bridge/ai-host-sessions.json, so a restart of the
//! hub keeps every host's session), so a host cannot tell which hub it talks to.
use bytes::Bytes;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Notify};

use pgm_core_internal::now_ms;
use pgm_net_internal::hostlink::{Envelope, HostKey};

pub const ENGINES: [&str; 4] = ["ollama", "image", "voice", "speech"];
const POLL_MS: u64 = 25000;
const JOB_MS: u64 = 300_000;
const PICKUP_MS: u64 = 40_000;
const IDLE_MS: u64 = 600_000;
const MAX_QUEUE: usize = 200;
const HOST_FILES: [&str; 9] = ["ai-node.js", "host-relay.js", "common.js", "worktree.js", "pc-tools-mcp.js", "board-mcp.js", "team-mcp.js", "vr-setup.js", "vr-host.js"];
const ADJ: [&str; 30] = ["happy", "silly", "brave", "quiet", "swift", "clever", "gentle", "bright", "lucky", "sleepy", "bold", "calm", "cosmic", "dapper", "eager", "fuzzy", "golden", "jolly", "kind", "merry", "nimble", "proud", "rusty", "shiny", "sunny", "tidy", "witty", "zesty", "mellow", "plucky"];
const ANIMAL: [&str; 30] = ["panda", "crocodile", "otter", "falcon", "badger", "walrus", "lynx", "heron", "gecko", "koala", "marmot", "narwhal", "ocelot", "puffin", "quokka", "raccoon", "sparrow", "tapir", "urchin", "viper", "wombat", "yak", "zebra", "beaver", "dolphin", "ferret", "ibis", "jackal", "kiwi", "moose"];
pub const OFFLINE_TEXT: &str = "Unity's AI host is offline: no AI host that lends her engines is connected (the machine is off or paused). Chat, the board, notes and Work keep working; Unity is back as soon as it is.";

/// An answer from a host: its status, headers and the body as it comes (whole, or piece by piece).
pub struct Relayed { pub status: u16, pub headers: Vec<(String, String)>, pub body: mpsc::Receiver<Result<Bytes, String>> }
impl Relayed {
    pub async fn bytes(mut self) -> Vec<u8> { let mut out = vec![]; while let Some(Ok(b)) = self.body.recv().await { out.extend_from_slice(&b); } out }
}
#[derive(Debug)]
pub struct HostErr { pub status: u16, pub msg: String }
fn herr(status: u16, m: impl Into<String>) -> HostErr { HostErr { status, msg: m.into() } }
pub enum WorkMsg { Event(Value), Done(i32), Error(String) }

enum Waiting { Whole(oneshot::Sender<Result<Relayed, HostErr>>), Stream(mpsc::Sender<Result<Bytes, String>>), Work(mpsc::UnboundedSender<WorkMsg>) }
struct Pending { kind: String, host: String, waiting: Option<Waiting>, token: String, account: String, display: String, provider: String, last: u64, picked: bool, project: String, started: u64 }

#[derive(Default)]
/// `limits`: the session limits its owner just set from the page (T-286), until its next hello says them itself.
struct Live { status: Value, status_at: u64, seen: u64, queue: Vec<Value>, waiters: usize, cancel: HashSet<String>, hello: Option<Value>, key: Option<HostKey>, kid: String, usage: Map<String, Value>, relay_ok: Map<String, Value>, notify: Arc<Notify>, limits: Option<Value> }

pub struct AiHosts {
    root: PathBuf,
    cfg: pgm_core_internal::config::Config,
    db: Mutex<Value>,
    legacy: Vec<Value>,
    live: Mutex<HashMap<String, Live>>,
    pending: Mutex<HashMap<String, Pending>>,
    was_up: Mutex<HashMap<String, (bool, u64)>>,
    offline_ms: u64,
    release_at: Mutex<(u64, String, String)>,
    /// engine → the host Unity found her brain on (`prefer_engine_host`, T-293)
    preferred: Mutex<HashMap<String, String>>,
    pub usage: Arc<crate::usage::Usage>,
    /// something people see changed (a hello, a host up / down, its usage, the registry): the JavaScript hub hears it
    pub dirty: Notify,
    /// The hub's one signing key (T-308, machine::sign): made once in the data folder (hub-signing.key, owner-only), signs
    /// every PC / machine request forwarded to a host. None only when the file could not be made (logged): requests then go unsigned.
    sign: Option<Arc<pgm_net_internal::machine::HubKey>>,
    /// Linked machines (T-311): the job queue their polls drain, the signed replies, big-file tickets and the transfer area.
    pub machines: Arc<crate::machine_jobs::MachineJobs>,
}
fn same_secret(a: &str, b: &str) -> bool { use subtle::ConstantTimeEq; let (x, y) = (pgm_core_internal::crypto::sha256(a.as_bytes()), pgm_core_internal::crypto::sha256(b.as_bytes())); bool::from(x.ct_eq(&y)) }
fn clean_users(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')).take(50).collect()).unwrap_or_default() }
/// What a shared provider is used for: Work runs, the /v1 API (and the products behind it), Unity's brain.
pub const USES: [&str; 3] = ["work", "api", "unity"];
/// What one person may use of a provider shared with them (5.11.0). `None` = everything (what sharing always meant).
/// Stored by the hub in the host record, `scopes[providerId][user | "@group"] = { models: "*" | [..], uses: "*" | [..] }`,
/// never on the host.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scope { pub models: Option<Vec<String>>, pub uses: Option<Vec<String>> }
impl Scope {
    pub fn restricted(&self) -> bool { self.models.is_some() || self.uses.is_some() }
    pub fn allows_use(&self, u: &str) -> bool { self.uses.as_ref().map(|l| l.iter().any(|x| x == u)).unwrap_or(true) }
    pub fn allows_model(&self, m: &str) -> bool { self.models.as_ref().map(|l| l.iter().any(|x| x == m)).unwrap_or(true) }
    /// One stored entry; a missing or "*" field means all.
    pub fn from_value(v: &Value) -> Scope {
        let list = |k: &str, only: Option<&[&str]>| v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).filter(|x| only.map(|o| o.contains(x)).unwrap_or(true)).map(String::from).collect::<Vec<_>>());
        Scope { models: list("models", None), uses: list("uses", Some(&USES)) }
    }
    pub fn to_value(&self) -> Value { json!({ "models": self.models.as_ref().map(|m| json!(m)).unwrap_or(json!("*")), "uses": self.uses.as_ref().map(|m| json!(m)).unwrap_or(json!("*")) }) }
}
/// The scope of `user` among one provider's stored scopes: the person's own entry wins over their group's ("@group", for
/// members of the hub's team group); nobody listed = everything.
pub fn scope_of(scopes: &Value, user: &str, in_group: bool) -> Scope {
    if scopes[user].is_object() { return Scope::from_value(&scopes[user]); }
    if in_group && scopes["@group"].is_object() { return Scope::from_value(&scopes["@group"]); }
    Scope::default()
}
/// Is this use of this model allowed? The error names the owner and what they allow.
pub fn check_scope(scope: &Scope, owner: &str, label: &str, use_: &str, model: &str) -> Result<(), String> {
    let who = if owner.is_empty() { "The owner".to_string() } else { owner.to_string() };
    if !scope.allows_use(use_) {
        let what = |u: &str| match u { "work" => "Work runs", "api" => "the API and products", "unity" => "Unity", o => o }.to_string();
        let ok: Vec<String> = scope.uses.clone().unwrap_or_default().iter().map(|u| what(u)).collect();
        return Err(format!("{who} shares {label} with you, but not for {}{}.", what(use_), if ok.is_empty() { String::new() } else { format!(" (they allow it for {})", ok.join(", ")) }));
    }
    if let Some(ms) = &scope.models {
        if ms.is_empty() { return Err(format!("{who} shares {label} with you with no model switched on.")); }
        if !model.is_empty() && !ms.iter().any(|m| m == model) { return Err(format!("{who} shares {label} with you, but not the model {model} (they allow {}).", ms.join(", "))); }
    }
    Ok(())
}
fn job_id() -> String { use base64::Engine; base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pgm_core_internal::crypto::random_bytes(9)) }
/// Does a host's hello list this model among any of its providers' models? ("unity-brain" = "unity-brain:latest".)
pub fn hello_has_model(hello: &Value, model: &str) -> bool {
    let want = model.trim_end_matches(":latest");
    hello["providers"].as_array().map(|ps| ps.iter().any(|p| p["models"].as_array().map(|ms| ms.iter().filter_map(Value::as_str).any(|m| m.trim_end_matches(":latest") == want)).unwrap_or(false))).unwrap_or(false)
}

impl AiHosts {
    pub fn new(root: &std::path::Path, cfg: &pgm_core_internal::config::Config, usage: Arc<crate::usage::Usage>) -> Arc<AiHosts> {
        let mut db: Value = std::fs::read_to_string(root.join("ai-hosts.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({ "hosts": {} }));
        if !db["hosts"].is_object() { db["hosts"] = json!({}); }
        let legacy: Vec<Value> = cfg.get("aiNodes").and_then(Value::as_object).map(|m| m.iter().filter(|(n, s)| !n.is_empty() && n.len() <= 40 && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && s.as_str().map(|s| s.len() >= 24).unwrap_or(false))
            .map(|(n, s)| json!({ "id": n, "user": "", "nickname": n, "secret": s, "legacy": true, "createdAt": Value::Null, "share": {} })).collect()).unwrap_or_default();
        let (version, commit) = release_now(root);
        let sign: Option<Arc<pgm_net_internal::machine::HubKey>> = match pgm_net_internal::machine::HubKey::load_or_create(&root.join("hub-signing.key")) { Ok(k) => Some(Arc::new(k)), Err(e) => { eprintln!("[ai-host] no hub signing key ({e}): PC requests to hosts go unsigned until it is fixed"); None } };
        let h = Arc::new(AiHosts { root: root.to_path_buf(), cfg: cfg.clone(), db: Mutex::new(db), legacy, live: Mutex::default(), pending: Mutex::default(), was_up: Mutex::default(),
            offline_ms: cfg.get("aiNodeOfflineMs").and_then(Value::as_u64).unwrap_or(60000).max(1000), release_at: Mutex::new((now_ms(), version, commit)), preferred: Mutex::default(), usage, dirty: Notify::new(),
            machines: Arc::new(crate::machine_jobs::MachineJobs::new(root, sign.clone(), cfg.get("machineTransferMaxMB").and_then(Value::as_u64).unwrap_or(4096))), sign });
        h.restore_sessions();
        h
    }
    fn save(&self) { let db = self.db.lock().unwrap_or_else(|e| e.into_inner()).clone(); let f = self.root.join("ai-hosts.json"); let tmp = f.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(&db).unwrap_or_default()).is_ok() { pgm_core_internal::fs::private(&tmp); let _ = std::fs::rename(&tmp, &f); } self.changed(); }
    pub fn record(&self, id: &str) -> Option<Value> { self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].get(id).cloned().or_else(|| self.legacy.iter().find(|r| r["id"] == json!(id)).cloned()) }
    fn all_records(&self) -> Vec<Value> { let mut v: Vec<Value> = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].as_object().map(|m| m.values().cloned().collect()).unwrap_or_default(); v.extend(self.legacy.clone()); v }
    // ---------- sessions, kept across restarts ----------
    fn restore_sessions(&self) {
        let s: Value = std::fs::read_to_string(self.root.join("ai-host-sessions.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        for (id, v) in s.as_object().cloned().unwrap_or_default() {
            if self.record(&id).is_none() || !v["hello"].is_object() { continue; }
            let Some(key) = v["key"].as_str().and_then(HostKey::from_hex) else { continue };
            let n = live.entry(id).or_default();
            n.kid = key.id(); n.key = Some(key); n.hello = Some(v["hello"].clone()); n.relay_ok = v["relayOk"].as_object().cloned().unwrap_or_default(); n.seen = now_ms();
        }
    }
    fn save_sessions(&self) {
        let o: Map<String, Value> = self.live.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(id, n)| n.hello.is_some() && n.key.is_some() && self.record(id).is_some())
            .map(|(id, n)| (id.clone(), json!({ "hello": n.hello, "key": n.key.as_ref().map(|k| k.to_hex()), "relayOk": n.relay_ok }))).collect();
        let f = self.root.join("ai-host-sessions.json"); let tmp = f.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(&Value::Object(o)).unwrap_or_default()).is_ok() { pgm_core_internal::fs::private(&tmp); let _ = std::fs::rename(&tmp, &f); }
        self.changed();
    }
    /// Online for jobs: connected and not paused by its owner (a host paused from the hub keeps listening, so only its
    /// status tells; the status is trusted for a minute).
    pub fn online(&self, id: &str) -> bool { self.live.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|n| (n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms) && !Self::paused_by_hub(n)).unwrap_or(false) }
    fn paused_by_hub(n: &Live) -> bool { n.status["paused"] == json!(true) && n.status["pausedBy"] == json!("hub") && now_ms().saturating_sub(n.status_at) < 60_000 }
    pub fn changed(&self) { self.dirty.notify_one(); }
    /// What the JavaScript hub needs to answer without asking (its Work runs, the API, the chat ask these all the time):
    /// each host's state, and what each account may pick. Sent on every change and every 5 s; online is counted there
    /// from seenMs + polling, so it never lags.
    pub fn snapshot(&self, users: &[String]) -> Value {
        let records = self.all_records();
        let mut hosts = Map::new(); let mut by_id = Map::new();
        {
            let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            for r in &records {
                let id = r["id"].as_str().unwrap_or(""); let n = live.get(id);
                let owner = { let u = r["user"].as_str().unwrap_or(""); if !u.is_empty() { u.to_string() } else { n.and_then(|n| n.hello.as_ref().and_then(|h| h["user"].as_str().map(String::from))).unwrap_or_default() } };
                hosts.insert(id.into(), json!({ "nickname": r["nickname"], "owner": owner, "seenMs": n.map(|n| n.seen).unwrap_or(0), "polling": n.map(|n| n.waiters > 0).unwrap_or(false), "hello": n.map(|n| n.hello.is_some() && n.key.is_some()).unwrap_or(false),
                    "pc": n.and_then(|n| n.hello.as_ref().map(|h| h["pc"] == json!(true))).unwrap_or(false), "engines": n.and_then(|n| n.hello.as_ref().map(|h| h["engines"].clone())).unwrap_or(json!(null)), "live": n.is_some(),
                    "providers": n.and_then(|n| n.hello.as_ref().map(|h| json!(h["providers"].as_array().map(|a| a.iter().map(|p| { let (c, until) = self.cap_of(id, n, p); json!({ "id": p["id"], "capped": c, "cappedUntil": until }) }).collect::<Vec<_>>()).unwrap_or_default()))).unwrap_or(json!([])) }));
                if let Some(n) = n { if let Some(h) = &n.hello { for p in h["providers"].as_array().cloned().unwrap_or_default() { let mut v = self.prov_view_locked(r, id, n, &p); let pid = format!("h:{id}/{}", p["id"].as_str().unwrap_or("")); v["id"] = json!(pid); by_id.insert(pid, v); } } }
            }
        }
        let mut providers = Map::new(); let mut mine = Map::new();
        for u in users {
            providers.insert(u.clone(), json!(self.providers_for(u).into_iter().map(|p| p["id"].clone()).collect::<Vec<_>>()));
            let m = self.mine(u); if !m.is_empty() { mine.insert(u.clone(), json!(m)); }
        }
        let engine = self.cfg.get("assistant").and_then(|a| a["node"].as_str()).unwrap_or("");
        json!({ "at": now_ms(), "offlineMs": self.offline_ms, "engineHost": engine, "hosts": hosts, "byId": by_id, "providers": providers, "mine": mine, "all": self.all(), "status": self.status(), "enabled": self.enabled() })
    }
    /// The up / down log, both ways (every 5 s).
    pub fn watch(self: &Arc<Self>) {
        let h = self.clone();
        tokio::spawn(async move { loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            for r in h.all_records() {
                let id = r["id"].as_str().unwrap_or("").to_string(); let up = h.online(&id);
                let prev = h.was_up.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
                match prev {
                    None => { if h.live.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&id) { h.was_up.lock().unwrap_or_else(|e| e.into_inner()).insert(id, (up, now_ms())); } }
                    Some((p, since)) if p != up => { println!("[ai-host] {} is {} ({} for {} s)", r["nickname"].as_str().unwrap_or(&id), if up { "back" } else { "OFFLINE" }, if up { "away" } else { "up" }, (now_ms() - since) / 1000); h.was_up.lock().unwrap_or_else(|e| e.into_inner()).insert(id, (up, now_ms())); }
                    _ => {}
                }
            }
            h.changed();
        } });
    }
    // ---------- what hosts offer ----------
    fn owner_of(&self, r: &Value) -> String { let u = r["user"].as_str().unwrap_or(""); if !u.is_empty() { return u.into(); } self.live.lock().unwrap_or_else(|e| e.into_inner()).get(r["id"].as_str().unwrap_or("")).and_then(|n| n.hello.as_ref().and_then(|h| h["user"].as_str().map(String::from))).unwrap_or_default() }
    fn allowed(r: &Value, p: &Value) -> Value { if r["share"][p["id"].as_str().unwrap_or("")].is_array() { r["share"][p["id"].as_str().unwrap_or("")].clone() } else { p["allowedUsers"].clone() } }
    fn usage_of(&self, id: &str, n: &Live, p: &Value) -> Value {
        let pid = p["id"].as_str().unwrap_or("");
        let seen = n.usage.get(pid).cloned().unwrap_or(json!({}));
        let kept = self.usage.windows(&format!("h:{id}/{pid}"));
        let pick = |k: &str| if !seen[k]["utilization"].is_null() { seen[k].clone() } else if !kept[k].is_null() { kept[k].clone() } else if !seen[k].is_null() { seen[k].clone() } else { Value::Null };
        json!({ "five_hour": pick("five_hour"), "seven_day": pick("seven_day") })
    }
    fn over_cap(&self, id: &str, n: &Live, p: &Value) -> String { self.cap_of(id, n, p).0 }
    /// Why the provider is capped ("" = it is not), and until when (the capping window's reset; 0 = not known).
    fn cap_of(&self, id: &str, n: &Live, p: &Value) -> (String, u64) {
        let u = self.usage_of(id, n, p); let now = now_ms() as f64;
        let pc = |w: &Value| -> f64 { match w["utilization"].as_f64() { Some(x) if w["resetsAt"].as_f64().map(|r| r > now).unwrap_or(true) => x * 100.0, _ => 0.0 } };
        let until = |w: &Value| w["resetsAt"].as_f64().unwrap_or(0.0) as u64;
        let (m5, m7) = (p["max5h"].as_f64().unwrap_or(90.0), p["max7d"].as_f64().unwrap_or(90.0));
        if pc(&u["five_hour"]) >= m5 { (format!("its 5-hour usage is at the {m5}% cap its host set"), until(&u["five_hour"])) }
        else if pc(&u["seven_day"]) >= m7 { (format!("its weekly usage is at the {m7}% cap its host set"), until(&u["seven_day"])) }
        else { (String::new(), 0) }
    }
    fn hub_kind(k: &str) -> &'static str { match k { "openai" => "host-openai", "codex-relay" => "host-codex", "jcode" => "host-jcode", _ => "host-claude" } }
    fn remote_kind(n: &Live, p: &Value) -> String { let k = p["kind"].as_str().unwrap_or(""); let pid = p["id"].as_str().unwrap_or(""); if k == "claude-cli" && n.relay_ok.get(pid) == Some(&json!(false)) { "claude-cli-work".into() } else { k.into() } }
    /// A provider as people see it (the live map is held by the caller).
    fn prov_view_locked(&self, r: &Value, id: &str, n: &Live, p: &Value) -> Value {
        let owner = { let u = r["user"].as_str().unwrap_or(""); if !u.is_empty() { u.to_string() } else { n.hello.as_ref().and_then(|h| h["user"].as_str().map(String::from)).unwrap_or_default() } };
        json!({ "id": format!("h:{id}/{}", p["id"].as_str().unwrap_or("")), "kind": Self::hub_kind(p["kind"].as_str().unwrap_or("")), "remoteKind": Self::remote_kind(n, p), "label": p["label"], "owner": owner, "shared": true, "models": p["models"],
            "hostId": id, "providerId": p["id"], "nickname": r["nickname"], "online": (n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms) && !Self::paused_by_hub(n), "max5h": p["max5h"], "max7d": p["max7d"], "usage": self.usage_of(id, n, p), "capped": self.over_cap(id, n, p), "cappedUntil": self.cap_of(id, n, p).1 })
    }
    /// The providers a person may pick from the hosts: "h:<hostId>/<providerId>".
    pub fn providers_for(&self, user: &str) -> Vec<Value> {
        let mut out = vec![];
        let records = self.all_records();
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        for r in records {
            let id = r["id"].as_str().unwrap_or("");
            let Some(n) = live.get(id) else { continue }; let Some(hello) = &n.hello else { continue };
            for p in hello["providers"].as_array().cloned().unwrap_or_default() { if self.may_use_locked(&r, &p, user, n) {
                let mut v = self.prov_view_locked(&r, id, n, &p);
                // only the models the owner allows this person; a provider with none left is not offered at all
                if !Self::scope_view(&mut v, &self.scope_locked(&r, &p, user, n)) { continue; }
                if let Some(o) = v.as_object_mut() { o.remove("providerId"); } out.push(v);
            } }
        }
        out
    }
    /// What the owner allows this person of that provider (the owner: everything).
    fn scope_locked(&self, r: &Value, p: &Value, user: &str, n: &Live) -> Scope {
        let owner = { let u = r["user"].as_str().unwrap_or(""); if !u.is_empty() { u.to_string() } else { n.hello.as_ref().and_then(|h| h["user"].as_str().map(String::from)).unwrap_or_default() } };
        if user == owner { return Scope::default(); }
        let in_group = clean_users(self.cfg.get("hostGroup").unwrap_or(&Value::Null)).iter().any(|x| x == user);
        scope_of(&r["scopes"][p["id"].as_str().unwrap_or("")], user, in_group)
    }
    /// Narrows a provider view to a scope (`models` cut to the allowed ones, `scope` says what is limited). False when no
    /// model is left.
    fn scope_view(v: &mut Value, sc: &Scope) -> bool {
        if !sc.restricted() { return true; }
        if let Some(ms) = &sc.models {
            let kept: Vec<Value> = v["models"].as_array().map(|a| a.iter().filter(|m| m.as_str().map(|m| ms.iter().any(|x| x == m)).unwrap_or(false)).cloned().collect()).unwrap_or_default();
            if kept.is_empty() { return false; }
            v["models"] = json!(kept);
        }
        v["scope"] = sc.to_value();
        true
    }
    /// May this person use this host provider ("h:<hostId>/<providerId>") for that use (work | api | unity) and model
    /// (empty = whichever the provider picks)? Anything that is not a host provider passes. The error names the owner.
    pub fn scope_check(&self, user: &str, pid: &str, use_: &str, model: &str) -> Result<(), String> {
        let Some((hid, prov)) = pid.strip_prefix("h:").and_then(|r| r.split_once('/')) else { return Ok(()) };
        if user.is_empty() { return Ok(()); }
        let Some(r) = self.record(hid) else { return Ok(()) };
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let Some(n) = live.get(hid) else { return Ok(()) };
        let Some(p) = n.hello.as_ref().and_then(|h| h["providers"].as_array().and_then(|a| a.iter().find(|x| x["id"] == json!(prov)).cloned())) else { return Ok(()) };
        let owner = { let u = r["user"].as_str().unwrap_or(""); if !u.is_empty() { u.to_string() } else { n.hello.as_ref().and_then(|h| h["user"].as_str().map(String::from)).unwrap_or_default() } };
        check_scope(&self.scope_locked(&r, &p, user, n), &owner, p["label"].as_str().unwrap_or(prov), use_, model)
    }
    /// May this person use that provider (the owner always; who the owner shares it with; "@group" = config hostGroup).
    fn may_use_locked(&self, r: &Value, p: &Value, user: &str, n: &Live) -> bool {
        if user.is_empty() { return false; }
        let owner = { let u = r["user"].as_str().unwrap_or(""); if !u.is_empty() { u.to_string() } else { n.hello.as_ref().and_then(|h| h["user"].as_str().map(String::from)).unwrap_or_default() } };
        if user == owner { return true; }
        let a = Self::allowed(r, p); let list: Vec<String> = a.as_array().map(|x| x.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
        list.iter().any(|x| x == user) || (list.iter().any(|x| x == "@group") && clean_users(self.cfg.get("hostGroup").unwrap_or(&Value::Null)).iter().any(|x| x == user))
    }
    /// One provider by id "h:<hostId>/<providerId>" (user "" = whoever may use it).
    pub fn provider(&self, pid: &str, user: &str) -> Option<Value> {
        let rest = pid.strip_prefix("h:")?; let (hid, prov) = rest.split_once('/')?;
        let r = self.record(hid)?; let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let n = live.get(hid)?; let hello = n.hello.as_ref()?;
        let p = hello["providers"].as_array()?.iter().find(|x| x["id"] == json!(prov))?.clone();
        if !user.is_empty() && !self.may_use_locked(&r, &p, user, n) { return None; }
        let mut v = self.prov_view_locked(&r, hid, n, &p); v["id"] = json!(pid);
        // narrowed to the person's scope; one with no model left stays (empty), so the use is refused with the owner's name
        if !user.is_empty() { let sc = self.scope_locked(&r, &p, user, n); if !Self::scope_view(&mut v, &sc) { v["models"] = json!([]); v["scope"] = sc.to_value(); } }
        Some(v)
    }
    /// The person's own AI host that offers their computer (the native app's "pc"), when it is online.
    /// Is one of this person's own AI hosts connected right now? (registered to them by the hub, as for PC tools: never
    /// the name a host says hello with)
    pub fn owns_online_host(&self, user: &str) -> bool {
        !user.is_empty() && self.all_records().iter().any(|r| r["user"].as_str() == Some(user) && self.online(r["id"].as_str().unwrap_or("")))
    }
    pub fn pc_host_for(&self, user: &str) -> Option<String> {
        if user.is_empty() { return None; }
        for r in self.all_records() {
            let id = r["id"].as_str().unwrap_or("").to_string();
            let pc = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(&id).and_then(|n| n.hello.as_ref().map(|h| h["pc"] == json!(true))).unwrap_or(false);
            if pc && self.owner_of(&r) == user && self.online(&id) { return Some(id); }
        }
        None
    }
    pub fn status(&self) -> Vec<Value> {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let was = self.was_up.lock().unwrap_or_else(|e| e.into_inner());
        self.all_records().iter().map(|r| {
            let id = r["id"].as_str().unwrap_or(""); let n = live.get(id);
            json!({ "id": id, "name": r["nickname"], "nickname": r["nickname"], "user": r["user"], "online": n.map(|n| n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms).unwrap_or(false), "polling": n.map(|n| n.waiters > 0).unwrap_or(false),
                "seen": n.filter(|n| n.seen > 0).map(|n| json!(pgm_core_internal::time::iso_at(n.seen as i64))).unwrap_or(Value::Null), "queued": n.map(|n| n.queue.len()).unwrap_or(0),
                "engines": n.and_then(|n| n.hello.as_ref().map(|h| h["engines"].clone())).unwrap_or(json!([])), "providers": n.and_then(|n| n.hello.as_ref().map(|h| json!(h["providers"].as_array().map(|a| a.iter().map(|p| p["id"].clone()).collect::<Vec<_>>()).unwrap_or_default()))).unwrap_or(json!([])),
                "caps": n.and_then(|n| n.hello.as_ref().map(|h| h["caps"].clone())).unwrap_or(json!([])), "kid": n.map(|n| n.kid.clone()).unwrap_or_default(), "version": n.and_then(|n| n.hello.as_ref().map(|h| h["version"].clone())).unwrap_or(json!("")), "since": was.get(id).map(|(_, s)| json!(pgm_core_internal::time::iso_at(*s as i64))).unwrap_or(Value::Null) })
        }).collect()
    }
    /// The hosts that may lend this engine right now, best first: the one pinned by config (`assistant.node`) alone when
    /// it is set; else every online host lending it that is the hub's own (config) or a hub admin's (Unity's engines
    /// see everyone's prompts, voice and pictures: never just anyone's), those whose hello lists Unity's brain among
    /// their models first (T-293), then by id, so the order is the same on every call.
    pub fn engine_hosts(&self, engine: &str) -> Vec<String> {
        if let Some(want) = self.cfg.get("assistant").and_then(|a| a["node"].as_str()).filter(|s| !s.is_empty()) {
            let ok = self.online(want) && self.live.lock().unwrap_or_else(|e| e.into_inner()).get(want).map(|n| n.hello.as_ref().map(|h| h["engines"].as_array().map(|a| a.iter().any(|e| e == engine)).unwrap_or(false)).unwrap_or(true)).unwrap_or(false);
            return if ok { vec![want.to_string()] } else { vec![] };
        }
        let brain = self.cfg.get("assistant").and_then(|a| a["model"].as_str()).filter(|s| !s.is_empty()).unwrap_or("unity-brain").to_string();
        let mut ids: Vec<(String, bool)> = self.live.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, n)| n.hello.as_ref().map(|h| h["engines"].as_array().map(|a| a.iter().any(|e| e == engine)).unwrap_or(false)).unwrap_or(true)).map(|(id, n)| (id.clone(), n.hello.as_ref().map(|h| hello_has_model(h, &brain)).unwrap_or(false))).collect();
        ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ids.into_iter().map(|(id, _)| id).filter(|id| self.online(id) && self.record(id).map(|r| r["legacy"] == json!(true) || self.owner_is_admin(&r)).unwrap_or(false)).collect()
    }
    /// Unity found out which host's Ollama has her brain (unity.rs asks each one's /api/tags, T-293): that host is asked
    /// first for this engine while it lends it; None forgets the choice.
    pub fn prefer_engine_host(&self, engine: &str, host: Option<String>) {
        let mut p = self.preferred.lock().unwrap_or_else(|e| e.into_inner());
        match host { Some(h) => { p.insert(engine.to_string(), h); } None => { p.remove(engine); } }
    }
    fn pick_engine_host(&self, engine: &str) -> Option<String> {
        let hosts = self.engine_hosts(engine);
        if let Some(p) = self.preferred.lock().unwrap_or_else(|e| e.into_inner()).get(engine) { if hosts.iter().any(|h| h == p) { return Some(p.clone()); } }
        hosts.into_iter().next()
    }

    fn owner_is_admin(&self, r: &Value) -> bool {
        let u = r["user"].as_str().unwrap_or(""); if u.is_empty() || !pgm_identity_internal::valid_username(u) { return false; }
        let def = self.cfg.get("defaultUsername").and_then(Value::as_str).unwrap_or("mills");
        let f = if u == def { self.root.join("config.json") } else { self.root.join("profiles").join(u).join("config.json") };
        std::fs::read_to_string(f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|c| c["admin"] == json!(true)).unwrap_or(false)
    }
    /// Why Unity's engines are away ('' while a host lends them).
    pub fn reason(&self) -> String { if self.pick_engine_host("ollama").is_some() { String::new() } else { OFFLINE_TEXT.into() } }

    // ---------- jobs ----------
    fn enqueue(self: &Arc<Self>, host: &str, job: Value, p: Pending) -> Result<(), HostErr> {
        let id = job["id"].as_str().unwrap_or("").to_string();
        let notify = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let n = live.entry(host.to_string()).or_default();
            if n.queue.len() >= MAX_QUEUE { return Err(herr(503, "That AI host is overloaded; try again in a moment.")); }
            n.queue.push(job); n.notify.clone()
        };
        let kind = p.kind.clone();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), p);
        notify.notify_waiters();
        // not picked up in time: the host is away
        let (h, id2, host2) = (self.clone(), id.clone(), host.to_string());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(PICKUP_MS)).await;
            let still = h.live.lock().unwrap_or_else(|e| e.into_inner()).get(&host2).map(|n| n.queue.iter().any(|j| j["id"] == json!(id2))).unwrap_or(false);
            if still { let nick = h.record(&host2).and_then(|r| r["nickname"].as_str().map(String::from)).unwrap_or(host2.clone()); h.finish(&id2, Some(if kind == "work" { format!("{nick} did not pick the job up: the host is away.") } else { OFFLINE_TEXT.to_string() }), None); }
        });
        Ok(())
    }
    /// Ends a job: its waiter gets the error, the end of its stream, or the run's end.
    fn finish(&self, id: &str, err: Option<String>, done_code: Option<i32>) {
        let Some(p) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id) else { return };
        if let Some(n) = self.live.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&p.host) { n.queue.retain(|j| j["id"] != json!(id)); n.cancel.remove(id); }
        match p.waiting {
            Some(Waiting::Whole(tx)) => { let _ = tx.send(Err(herr(503, err.unwrap_or_else(|| "cancelled".into())))); }
            Some(Waiting::Stream(tx)) => { if let Some(e) = err { let _ = tx.try_send(Err(e)); } }
            Some(Waiting::Work(tx)) => { let _ = tx.send(match err { Some(e) => WorkMsg::Error(e), None => WorkMsg::Done(done_code.unwrap_or(0)) }); }
            None => {}
        }
    }
    /// One call carried to a host (engine, endpoint, login relay, PC tool): its answer as it comes.
    /// all_headers: a login relay (the host keeps what the provider needs); everything else carries only its content type.
    pub async fn relay(self: &Arc<Self>, host: &str, engine: &str, path: &str, method: &str, headers: Vec<(String, String)>, body: Vec<u8>, kind: &str, all_headers: bool) -> Result<Relayed, HostErr> {
        let id = job_id();
        let mut h = Map::new();
        h.insert("content-type".into(), json!(""));
        for (k, v) in headers { let k = k.to_lowercase(); if all_headers || k == "content-type" { h.insert(k, json!(v)); } }
        let job = json!({ "id": id, "kind": kind, "engine": engine, "path": path, "method": method, "headers": h, "body": if body.is_empty() { String::new() } else { pgm_core_internal::b64(&body) } });
        let (tx, rx) = oneshot::channel();
        self.enqueue(host, job, Pending { kind: kind.into(), host: host.into(), waiting: Some(Waiting::Whole(tx)), token: String::new(), account: String::new(), display: String::new(), provider: String::new(), last: now_ms(), picked: false, project: String::new(), started: now_ms() })?;
        let (hh, id2) = (self.clone(), id.clone());
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(JOB_MS)).await; hh.finish(&id2, Some("The AI host did not answer in time.".into()), None); });
        // the caller went away before the answer came (the page or the JavaScript hub hung up): the job ends with it
        struct Gone<'a>(&'a AiHosts, &'a str, bool);
        impl Drop for Gone<'_> { fn drop(&mut self) { if self.2 { self.0.finish(self.1, Some("cancelled".into()), None); } } }
        let mut g = Gone(self, &id, true);
        let r = rx.await; g.2 = false;
        match r { Ok(r) => r, Err(_) => Err(herr(503, "cancelled")) }
    }
    pub async fn engine_fetch(self: &Arc<Self>, engine: &str, path: &str, method: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<Relayed, HostErr> {
        if !ENGINES.contains(&engine) { return Err(herr(400, format!("Unknown engine {engine}"))); }
        let id = self.pick_engine_host(engine).ok_or_else(|| herr(503, OFFLINE_TEXT))?;
        self.relay(&id, engine, path, method, headers, body, "engine", false).await
    }
    /// The same call to one named host (one of `engine_hosts`): how Unity asks each Ollama what it has (T-293).
    pub async fn engine_fetch_on(self: &Arc<Self>, host: &str, engine: &str, path: &str, method: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<Relayed, HostErr> {
        if !ENGINES.contains(&engine) { return Err(herr(400, format!("Unknown engine {engine}"))); }
        if !self.engine_hosts(engine).iter().any(|h| h == host) { return Err(herr(503, format!("{host} does not lend the {engine} engine right now."))); }
        self.relay(host, engine, path, method, headers, body, "engine", false).await
    }
    pub async fn provider_fetch(self: &Arc<Self>, host: &str, pid: &str, path: &str, method: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<Relayed, HostErr> {
        if !self.online(host) { return Err(herr(503, format!("That AI host ({}) is offline right now.", self.record(host).and_then(|r| r["nickname"].as_str().map(String::from)).unwrap_or(host.into())))); }
        self.relay(host, &format!("provider:{pid}"), path, method, headers, body, "provider", false).await
    }
    /// A model call of this hub's own Claude Code / Codex, carried to the host that adds its login. Usage headers feed its caps.
    pub async fn login_fetch(self: &Arc<Self>, host: &str, pid: &str, path: &str, method: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<Relayed, HostErr> {
        if !self.online(host) { return Err(herr(503, format!("That AI host ({}) is offline right now.", self.record(host).and_then(|r| r["nickname"].as_str().map(String::from)).unwrap_or(host.into())))); }
        let cap = { let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); live.get(host).and_then(|n| n.hello.as_ref().and_then(|h| h["providers"].as_array().and_then(|a| a.iter().find(|p| p["id"] == json!(pid)).cloned())).map(|p| self.over_cap(host, n, &p))).unwrap_or_default() };
        if !cap.is_empty() { return Err(herr(429, format!("{}: {cap}.", self.record(host).and_then(|r| r["nickname"].as_str().map(String::from)).unwrap_or_default()))); }
        let hs: Vec<(String, String)> = headers.into_iter().filter(|(k, _)| !matches!(k.to_lowercase().as_str(), "authorization" | "x-api-key" | "host" | "content-length" | "connection" | "cookie")).collect();
        let r = self.relay(host, &format!("relay:{pid}"), path, method, hs, body, "provider", true).await?;
        self.note_headers(host, pid, &r.headers);
        Ok(r)
    }
    fn note_headers(&self, host: &str, pid: &str, h: &[(String, String)]) {
        let g = |n: &str| h.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone());
        let w = |name: &str| -> Value { match g(&format!("anthropic-ratelimit-unified-{name}-utilization")) { Some(u) => json!({ "utilization": u.parse::<f64>().ok(), "resetsAt": g(&format!("anthropic-ratelimit-unified-{name}-reset")).and_then(|r| r.parse::<f64>().ok()).map(|r| r * 1000.0), "status": g(&format!("anthropic-ratelimit-unified-{name}-status")).unwrap_or_default() }), None => Value::Null } };
        let (five, seven) = (w("5h"), w("7d"));
        if five.is_null() && seven.is_null() { return; }
        let prev = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(host).and_then(|n| n.usage.get(pid).cloned()).unwrap_or(json!({}));
        self.note_host_usage(host, pid, &json!({ "five_hour": if five.is_null() { prev["five_hour"].clone() } else { five }, "seven_day": if seven.is_null() { prev["seven_day"].clone() } else { seven } }));
    }
    fn note_host_usage(&self, host: &str, pid: &str, u: &Value) {
        let clean = |w: &Value| -> Value { if !w.is_object() { return Value::Null; } let n = w["utilization"].as_f64().or_else(|| w["utilization"].as_str().and_then(|s| s.parse().ok())); json!({ "utilization": n.map(|x| (if x > 1.0 { x / 100.0 } else { x }).clamp(0.0, 1.0)), "resetsAt": w["resetsAt"].as_f64(), "status": w["status"].as_str().unwrap_or("") }) };
        let rec = json!({ "five_hour": clean(&u["five_hour"]), "seven_day": clean(&u["seven_day"]), "at": now_ms() });
        if let Some(n) = self.live.lock().unwrap_or_else(|e| e.into_inner()).get_mut(host) { n.usage.insert(pid.to_string(), rec.clone()); }
        self.usage.note_windows(&format!("h:{host}/{pid}"), &rec);
        self.changed();
    }
    /// The account the hub registered a host to (Settings → My AI hosts), "" for none. Never the name a host says it has.
    fn registered_owner(r: &Value) -> String { r["user"].as_str().unwrap_or("").to_string() }
    /// A machine the caller NAMED (its id or nickname), for the account that is calling (T-308, plans/linked-machines.md
    /// section 4.3): only that machine's registered owner may use it. Another person's machine is refused with 403 and no
    /// fallback to anything else, whoever asks, an admin or another person's run included. Ok = the host id.
    pub fn machine_for(&self, named: &str, account: &str) -> Result<String, (u16, String)> {
        let n = named.trim().to_lowercase();
        let found = if n.is_empty() { None } else { self.all_records().into_iter().find(|r| r["id"].as_str().map(|i| i.to_lowercase() == n).unwrap_or(false) || r["nickname"].as_str().map(|i| i.to_lowercase() == n).unwrap_or(false)) };
        let Some(r) = found else { return Err((404, format!("There is no machine called {named}."))) };
        let owner = Self::registered_owner(&r);
        if account.is_empty() || owner.is_empty() || owner != account { return Err((403, "That machine belongs to someone else.".into())); }
        let id = r["id"].as_str().unwrap_or("").to_string();
        let nick = r["nickname"].as_str().unwrap_or(&id).to_string();
        let pc = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(&id).and_then(|n| n.hello.as_ref().map(|h| h["pc"] == json!(true))).unwrap_or(false);
        if !self.online(&id) { return Err((503, format!("Your machine {nick} is offline right now."))); }
        if !pc { return Err((400, format!("Your machine {nick} does not offer computer access (no pc.folder set on it)."))); }
        Ok(id)
    }
    /// A PC tool on a host, SIGNED by the hub for the machine's owner and the actor (machine::sign): the host verifies it when it
    /// knows the hub's key (hello, cap `signed-pc`); an older host ignores the header and works as before. The hub's own check
    /// is the first line: this is the second, so a bug in a caller can never sign a request for someone else's machine.
    pub async fn pc_tool(self: &Arc<Self>, host: &str, name: &str, args: &Value, actor: &pgm_net_internal::machine::Actor) -> Result<Value, HostErr> {
        let owner = self.record(host).map(|r| Self::registered_owner(&r)).unwrap_or_default();
        if owner.is_empty() || owner != actor.account { return Err(herr(403, "That machine belongs to someone else.")); }
        let body = args.to_string().into_bytes();
        let mut headers: Vec<(String, String)> = vec![("content-type".into(), "application/json".into())];
        if let Some(k) = &self.sign {
            let env = pgm_net_internal::machine::Envelope::new(&owner, actor.clone(), name, &body, host, (now_ms() / 1000) as i64);
            headers.push((pgm_net_internal::machine::sign::HEADER.into(), k.sign(&env).to_header()));
        }
        let r = self.relay(host, "pc", &format!("/{name}"), "POST", headers, body, "pc", true).await?;
        let b = r.bytes().await;
        serde_json::from_slice(&b).map_err(|e| herr(502, e.to_string()))
    }
    /// The frozen JavaScript hub's PC call (`/__front/ai/pc-tool`): it already chose the person's own host, so the actor is
    /// that host's registered owner (compatibility; the JavaScript hub never names a run).
    pub async fn pc_tool_front(self: &Arc<Self>, host: &str, name: &str, args: &Value) -> Result<Value, HostErr> {
        let owner = self.record(host).map(|r| Self::registered_owner(&r)).unwrap_or_default();
        self.pc_tool(host, name, args, &pgm_net_internal::machine::Actor { account: owner, session: String::new(), run: "js-hub".into() }).await
    }
    /// A Work run on a host (2.x hosts and hosts that allow Work): Claude Code runs there; events come back sealed.
    pub fn run_work(self: &Arc<Self>, host: &str, pid: &str, payload: Value) -> Result<(String, String, mpsc::UnboundedReceiver<WorkMsg>), HostErr> {
        let r = self.record(host).ok_or_else(|| herr(503, "That AI host has not connected yet."))?;
        let nick = r["nickname"].as_str().unwrap_or(host).to_string();
        let (key, prov, caps) = { let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let n = live.get(host).ok_or_else(|| herr(503, "That AI host has not connected yet."))?; (n.key.clone(), n.hello.as_ref().and_then(|h| h["providers"].as_array().and_then(|a| a.iter().find(|p| p["id"] == json!(pid)).cloned())), n.hello.as_ref().map(|h| h["caps"].clone()).unwrap_or(Value::Null)) };
        let key = key.ok_or_else(|| herr(503, "That AI host has not connected yet."))?;
        // a jcode Work job goes only to a host that announced it can run one
        if prov.as_ref().map(|p| p["kind"] == json!("jcode")).unwrap_or(false) && !caps.as_array().map(|c| c.iter().any(|x| x == pgm_net_internal::hostlink::CAP_WORK_JCODE)).unwrap_or(false) {
            return Err(herr(400, "That AI host did not announce that it runs Work through Jcode PGM (work-jcode); update it or pick another AI."));
        }
        if !self.online(host) { let o = self.owner_of(&r); return Err(herr(503, format!("{nick}{} is offline right now. Pick another AI in the dropdown, or wait for it.", if o.is_empty() { " (AI host)".to_string() } else { format!(" ({o}'s AI host)") }))); }
        let prov = prov.ok_or_else(|| herr(503, "That host no longer offers this provider."))?;
        let cap = { let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); live.get(host).map(|n| self.over_cap(host, n, &prov)).unwrap_or_default() };
        if !cap.is_empty() { return Err(herr(429, format!("{nick}: {cap}. Pick another AI or wait for the window to reset."))); }
        let id = job_id(); let token = pgm_core_internal::crypto::random_hex(24);
        let mut p = payload.clone(); p["providerId"] = json!(pid); p["token"] = json!(token); p["mcpBase"] = json!(format!("{}/node/job/{id}", payload["hubUrl"].as_str().unwrap_or("")));
        let enc = key.seal(&p).map_err(|e| herr(500, e.to_string()))?;
        let (tx, rx) = mpsc::unbounded_channel();
        self.enqueue(host, json!({ "id": id, "kind": "work", "enc": enc }), Pending { kind: "work".into(), host: host.into(), waiting: Some(Waiting::Work(tx)), token: token.clone(), account: payload["account"].as_str().unwrap_or("").into(), display: payload["display"].as_str().unwrap_or("").into(), provider: pid.into(), last: now_ms(), picked: false, project: payload["project"].as_str().unwrap_or("").chars().take(64).collect(), started: now_ms() })?;
        // no word for ten minutes: given up
        let (h, id2) = (self.clone(), id.clone());
        tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(30)).await; let last = h.pending.lock().unwrap_or_else(|e| e.into_inner()).get(&id2).map(|p| p.last); match last { Some(l) if now_ms() - l > IDLE_MS => { h.finish(&id2, Some("No word from the AI host for ten minutes; the run was given up.".into()), None); return; } Some(_) => {} None => return } } });
        Ok((id, token, rx))
    }
    pub fn cancel_work(&self, id: &str) { let host = self.pending.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|p| p.host.clone()); if let Some(h) = host { let n = { let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner()); live.get_mut(&h).map(|n| { n.cancel.insert(id.to_string()); n.notify.clone() }) }; if let Some(n) = n { n.notify_waiters(); } } }
    /// A host's Work run calling the team tools: its account, when the token is the run's.
    pub fn job_for(&self, id: &str, token: &str) -> Option<(String, String)> { let p = self.pending.lock().unwrap_or_else(|e| e.into_inner()); let j = p.get(id)?; if j.kind == "work" && !token.is_empty() && same_secret(token, &j.token) { Some((j.account.clone(), j.display.clone())) } else { None } }

    // ---------- older hosts: can this one relay a login, or does its Claude run Work itself (2.x)? ----------
    fn probe_relays(self: &Arc<Self>, id: String) {
        let h = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let provs: Vec<String> = h.live.lock().unwrap_or_else(|e| e.into_inner()).get(&id).and_then(|n| n.hello.as_ref().map(|hh| hh["providers"].as_array().map(|a| a.iter().filter(|p| p["kind"] == json!("claude-cli") || p["kind"] == json!("claude-relay")).filter_map(|p| p["id"].as_str().map(String::from)).collect()).unwrap_or_default())).unwrap_or_default();
            for pid in provs {
                let Ok(r) = h.relay(&id, &format!("relay:{pid}"), "/v1/__viberoller_probe", "POST", vec![("content-type".into(), "application/json".into())], b"{}".to_vec(), "provider", true).await else { continue };
                let status = r.status; let text = String::from_utf8_lossy(&r.bytes().await).to_lowercase();
                let ok = !(status == 404 && text.contains("does not lend"));
                if let Some(n) = h.live.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) { n.relay_ok.insert(pid.clone(), json!(ok)); }
                h.save_sessions();
                if !ok { println!("[ai-host] {} is an older host (no login relay): its Claude runs Work on that machine, as it did before", h.record(&id).and_then(|r| r["nickname"].as_str().map(String::from)).unwrap_or(id.clone())); }
            }
        });
    }
    // ---------- the hosts' own routes ----------
    fn who(&self, hs: &hyper::HeaderMap) -> Option<Value> {
        let g = |k: &str| hs.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let id = { let a = g("x-vr-host"); if a.is_empty() { g("x-vr-node") } else { a } };
        let tok = { let a = g("x-vr-host-token"); if a.is_empty() { g("x-vr-node-token") } else { a } };
        let r = self.record(&id)?;
        if tok.is_empty() || !same_secret(&tok, r["secret"].as_str().unwrap_or("")) { return None; }
        Some(r)
    }
    /// What a host offered, cleaned. A `jcode` provider (Work through Jcode PGM on the host) is kept only when the host
    /// announced `work-jcode` in its caps: without it the hub never lists one, so never sends it a jcode job.
    fn clean_providers(list: &Value, caps: &[String]) -> Vec<Value> {
        let mut out: Vec<Value> = vec![];
        let pct = |v: &Value, d: i64| v.as_f64().filter(|n| *n > 0.0 && *n <= 100.0).map(|n| n.round() as i64).unwrap_or(d);
        let work_jcode = caps.iter().any(|c| c == pgm_net_internal::hostlink::CAP_WORK_JCODE);
        for p in list.as_array().cloned().unwrap_or_default().into_iter().take(20) {
            let id = p["id"].as_str().unwrap_or("").trim().to_string(); let kind = p["kind"].as_str().unwrap_or("");
            if id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') || !["claude-cli", "claude-relay", "codex-relay", "anthropic", "openai"].contains(&kind) && !(kind == "jcode" && work_jcode) || out.iter().any(|x| x["id"] == json!(id)) { continue; }
            let models: Vec<String> = p["models"].as_array().map(|a| a.iter().filter_map(|m| m.as_str()).map(|m| m.trim().to_string()).filter(|m| !m.is_empty() && m.len() <= 80 && m.chars().all(|c| c.is_ascii_alphanumeric() || "_.:/-".contains(c))).take(30).collect()).unwrap_or_default();
            let mut users: Vec<Value> = clean_users(&p["allowedUsers"]).into_iter().map(Value::String).collect();
            if p["allowedUsers"].as_array().map(|a| a.iter().any(|x| x == "@group")).unwrap_or(false) { users.push(json!("@group")); }
            let label: String = p["label"].as_str().unwrap_or(&id).trim().chars().take(60).collect();
            out.push(json!({ "id": id, "kind": kind, "label": if label.is_empty() { id.clone() } else { label }, "models": models, "allowedUsers": users, "max5h": pct(&p["max5h"], 90), "max7d": pct(&p["max7d"], 90) }));
        }
        out
    }
    /// The version and commit this hub runs (looked at again every 30 s: the JavaScript hub restarts onto new code while
    /// this front keeps running).
    pub fn release(&self) -> (String, String) {
        let mut r = self.release_at.lock().unwrap_or_else(|e| e.into_inner());
        if now_ms() - r.0 > 30_000 { let (v, c) = release_now(&self.root); *r = (now_ms(), v, c); }
        (r.1.clone(), r.2.clone())
    }
    fn beat(&self) -> Value { let (v, c) = self.release(); json!({ "t": now_ms(), "version": v, "commit": c }) }
    /// The /node/* routes: (status, answer) or a raw body for a file.
    pub async fn route(self: &Arc<Self>, method: &hyper::Method, path: &str, query: &str, hs: &hyper::HeaderMap, body: &[u8], native: &pgm_dist_internal::releases::Releases) -> (u16, Result<Value, (Vec<u8>, &'static str)>) {
        let Some(r) = self.who(hs) else { return (401, Ok(json!({ "error": "Unknown AI host." }))) };
        let id = r["id"].as_str().unwrap_or("").to_string();
        let b: Value = serde_json::from_slice(body).unwrap_or(json!({}));
        let notify = { let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let n = live.entry(id.clone()).or_default(); n.seen = now_ms(); n.notify.clone() };
        let get = *method == hyper::Method::GET; let post = *method == hyper::Method::POST;
        match path {
            "/node/hello" if post => {
                let nonce = b["nonce"].as_str().unwrap_or("").to_string();
                if !(32..=64).contains(&nonce.len()) || !nonce.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) { return (400, Ok(json!({ "error": "hello needs a hex nonce (32+ characters)." }))); }
                let server_nonce = pgm_core_internal::crypto::random_hex(16);
                let Ok(key) = HostKey::derive(r["secret"].as_str().unwrap_or(""), &nonce, &server_nonce) else { return (400, Ok(json!({ "error": "bad nonce" }))) };
                let caps: Vec<String> = b["caps"].as_array().map(|a| a.iter().filter_map(|c| c.as_str()).filter(|c| !c.is_empty() && c.len() <= 20 && c.bytes().all(|x| x.is_ascii_lowercase() || x == b'-')).take(20).map(String::from).collect()).unwrap_or_default();
                let engines: Vec<String> = match &b["engines"] { Value::Array(a) => a.iter().filter_map(|e| e.as_str().map(String::from)).collect(), Value::Object(m) => m.keys().cloned().collect(), _ => vec![] };
                let engines: Vec<String> = engines.into_iter().filter(|e| ENGINES.contains(&e.as_str())).collect();
                let user = { let u = r["user"].as_str().unwrap_or(""); if u.is_empty() { b["user"].as_str().unwrap_or("").to_string() } else { u.to_string() } };
                // its computer (PC tools) only for the owner the hub registered it to (Settings → My AI hosts / "Run on
                // this PC"), never for a name a host says it belongs to: only Gee works on Gee's computer
                let pc = b["pc"] == json!(true) && !r["user"].as_str().unwrap_or("").is_empty();
                let hello = json!({ "at": now_ms(), "nickname": r["nickname"], "user": user, "providers": Self::clean_providers(&b["providers"], &caps), "engines": engines, "version": b["version"].as_str().unwrap_or("").chars().take(20).collect::<String>(), "os": b["os"].as_str().unwrap_or("").chars().take(20).collect::<String>(), "pc": pc, "caps": caps, "sessionLimits": clean_limits(&b["sessionLimits"]) });
                let kid = key.id();
                { let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let n = live.entry(id.clone()).or_default(); n.key = Some(key); n.kid = kid.clone(); n.limits = None; n.hello = Some(hello.clone()); n.relay_ok = Map::new(); }
                self.save_sessions();
                println!("[ai-host] {} ({}) connected: {} provider(s), engines {}, key {kid}{}", r["nickname"].as_str().unwrap_or(""), if hello["user"].as_str().unwrap_or("").is_empty() { "no owner" } else { hello["user"].as_str().unwrap_or("") }, hello["providers"].as_array().map(|a| a.len()).unwrap_or(0), if hello["engines"].as_array().map(|a| a.is_empty()).unwrap_or(true) { "none".to_string() } else { hello["engines"].as_array().unwrap().iter().filter_map(|e| e.as_str()).collect::<Vec<_>>().join(", ") }, if caps.is_empty() { ", an older host".to_string() } else { format!(", can {}", caps.join(" ")) });
                if !caps.iter().any(|c| c == "relay") { self.probe_relays(id.clone()); }
                (200, Ok(json!({ "id": id, "nickname": r["nickname"], "user": r["user"], "serverNonce": server_nonce, "kid": kid, "pollMs": POLL_MS, "offlineMs": self.offline_ms, "hubKey": self.sign.as_ref().map(|k| k.public_hex()).unwrap_or_default(), "hubKid": self.sign.as_ref().map(|k| k.kid()).unwrap_or_default(), "machine": { "v": pgm_net_internal::compat::MACHINE_V }, "group": clean_users(self.cfg.get("hostGroup").unwrap_or(&Value::Null)), "update": { "version": self.release().0, "commit": self.release().1 } })))
            }
            // how the host is doing (cap `control`): kept for the owner's page, nothing more
            "/node/status" if post => {
                let st = clean_status(&b);
                if let Some(n) = self.live.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) { n.status = st; n.status_at = now_ms(); }
                self.changed();
                (200, Ok(json!({ "ok": true })))
            }
            "/node/update" if get => match self.host_files_manifest() {
                Some(m) => { let sig = pgm_core_internal::crypto::hmac_hex(r["secret"].as_str().unwrap_or("").as_bytes(), m.to_string().as_bytes()); let mut out = m.clone(); out["sig"] = json!(sig); (200, Ok(out)) }
                None => (404, Ok(json!({ "error": "No updates here." }))),
            },
            p if get && p.starts_with("/node/update/file/") => {
                let name = &p["/node/update/file/".len()..];
                if !HOST_FILES.contains(&name) { return (404, Ok(json!({ "error": "Not a host file." }))); }
                match std::fs::read(self.root.join(name)) { Ok(buf) => (200, Err((buf, "application/octet-stream"))), Err(_) => (404, Ok(json!({ "error": "Not here." }))) }
            }
            "/node/update/bin" | "/node/update/bin/file" if get => {
                let q: HashMap<String, String> = pgm_net_internal::http::url_params(query).into_iter().collect();
                let app = q.get("app").cloned().unwrap_or_else(|| "vr-host".into()); let target = q.get("target").cloned().unwrap_or_default();
                if app != "vr-host" { return (404, Ok(json!({ "error": "No native releases here." }))); }
                if path == "/node/update/bin" { match native.signed(&app, &target, Some(r["secret"].as_str().unwrap_or("")), &self.release()) { Some(s) => (200, Ok(s)), None => (404, Ok(json!({ "error": format!("Nothing built for {target}.") }))) } }
                else { match native.file(&app, &target, q.get("name").map(String::as_str).unwrap_or("")) { Some(buf) => (200, Err((buf, "application/octet-stream"))), None => (404, Ok(json!({ "error": "Not a file of that release." }))) } }
            }
            "/node/poll" if get => {
                let has_hello = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(&id).map(|n| n.hello.is_some()).unwrap_or(false);
                if !has_hello && hs.contains_key("x-vr-host") { return (if hs.contains_key("x-vr-host-version") { 428 } else { 401 }, Ok(json!({ "error": "Say hello again: this hub does not know what you offer yet.", "rehello": true }))); }
                // listening before looking, so a job queued in between is not missed
                let woke = notify.notified(); tokio::pin!(woke); woke.as_mut().enable();
                let ready = { let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); live.get(&id).map(|n| !n.queue.is_empty() || !n.cancel.is_empty()).unwrap_or(false) };
                if !ready {
                    // counted as polling while it waits, also when the host hangs up halfway (the guard counts it out)
                    struct Waiter<'a>(&'a AiHosts, String);
                    impl Drop for Waiter<'_> { fn drop(&mut self) { if let Some(n) = self.0.live.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&self.1) { n.waiters = n.waiters.saturating_sub(1); n.seen = now_ms(); } self.0.changed(); } }
                    { self.live.lock().unwrap_or_else(|e| e.into_inner()).entry(id.clone()).or_default().waiters += 1; }
                    let _w = Waiter(self, id.clone());
                    let _ = tokio::time::timeout(Duration::from_millis(POLL_MS), woke).await;
                }
                let (jobs, cancel) = { let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let n = live.entry(id.clone()).or_default(); let k = n.queue.len().min(8); let jobs: Vec<Value> = n.queue.drain(0..k).collect(); let cancel: Vec<String> = n.cancel.drain().collect(); (jobs, cancel) };
                { let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner()); for j in &jobs { if let Some(pj) = p.get_mut(j["id"].as_str().unwrap_or("")) { pj.picked = true; } } }
                (200, Ok(json!({ "jobs": jobs, "cancel": cancel, "beat": self.beat() })))
            }
            "/node/reply" if post => {
                let jid = b["id"].as_str().unwrap_or("").to_string();
                let mut pend = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                let ok = pend.get(&jid).map(|j| j.host == id && j.kind != "work" && matches!(j.waiting, Some(Waiting::Whole(_)))).unwrap_or(false);
                if !ok { return (404, Ok(json!({ "error": "No such job." }))); }
                let j = pend.remove(&jid).unwrap(); drop(pend);
                if let Some(n) = self.live.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) { n.queue.retain(|x| x["id"] != json!(jid)); }
                let body = pgm_core_internal::unb64(b["body"].as_str().unwrap_or("")).unwrap_or_default();
                let (tx, rx) = mpsc::channel(2); if !body.is_empty() { let _ = tx.try_send(Ok(Bytes::from(body))); } drop(tx);
                if let Some(Waiting::Whole(w)) = j.waiting { let _ = w.send(Ok(Relayed { status: host_status(&b["status"]), headers: pick_headers(&b["headers"]), body: rx })); }
                (200, Ok(json!({ "ok": true })))
            }
            "/node/stream" if post => {
                let jid = b["id"].as_str().unwrap_or("").to_string();
                let (sender, first) = {
                    let mut pend = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(j) = pend.get_mut(&jid).filter(|j| j.host == id && j.kind != "work") else { return (404, Ok(json!({ "error": "No such job." }))) };
                    match j.waiting.take() {
                        Some(Waiting::Whole(w)) => { let (tx, rx) = mpsc::channel(256); let _ = w.send(Ok(Relayed { status: host_status(&b["status"]), headers: pick_headers(&b["headers"]), body: rx })); j.waiting = Some(Waiting::Stream(tx.clone())); (Some(tx), true) }
                        Some(Waiting::Stream(tx)) => { j.waiting = Some(Waiting::Stream(tx.clone())); (Some(tx), false) }
                        other => { j.waiting = other; (None, false) }
                    }
                };
                let _ = first;
                if let (Some(tx), Some(c)) = (&sender, b["chunk"].as_str()) { if tx.send(Ok(Bytes::from(pgm_core_internal::unb64(c).unwrap_or_default()))).await.is_err() { self.finish(&jid, Some("cancelled".into()), None); return (404, Ok(json!({ "error": "No longer wanted." }))); } }
                if b["done"] == json!(true) { let e = b["error"].as_str().map(String::from); if let Some(p) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&jid) { if let (Some(Waiting::Stream(tx)), Some(e)) = (p.waiting, e) { let _ = tx.try_send(Err(e)); } } }
                (200, Ok(json!({ "ok": true })))
            }
            "/node/events" if post => {
                let jid = b["job"].as_str().unwrap_or("").to_string();
                let (valid, prov) = { let p = self.pending.lock().unwrap_or_else(|e| e.into_inner()); p.get(&jid).map(|j| (j.host == id && j.kind == "work", j.provider.clone())).unwrap_or((false, String::new())) };
                if !valid { return (404, Ok(json!({ "error": "No such job." }))); }
                let key = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(&id).and_then(|n| n.key.clone());
                let env: Option<Envelope> = serde_json::from_value(b["enc"].clone()).ok();
                let Some(batch) = key.zip(env).and_then(|(k, e)| k.open(&e).ok()) else { return (400, Ok(json!({ "error": "Could not open the envelope (the keys differ: say hello again)." }))) };
                if let Some(p) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&jid) { p.last = now_ms(); }
                if batch["usage"].is_object() { self.note_host_usage(&id, &prov, &batch["usage"]); }
                let tx = { let p = self.pending.lock().unwrap_or_else(|e| e.into_inner()); p.get(&jid).and_then(|j| if let Some(Waiting::Work(t)) = &j.waiting { Some(t.clone()) } else { None }) };
                if let Some(tx) = &tx { for ev in batch["events"].as_array().cloned().unwrap_or_default() { let _ = tx.send(WorkMsg::Event(ev)); } }
                let cancelled = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(&id).map(|n| n.cancel.contains(&jid)).unwrap_or(false);
                if batch["done"] == json!(true) { self.finish(&jid, None, Some(batch["code"].as_i64().unwrap_or(0) as i32)); }
                (200, Ok(json!({ "ok": true, "cancel": cancelled })))
            }
            _ => (404, Ok(json!({ "error": "Not found" }))),
        }
    }
    /// The JavaScript host's files as this hub runs them (name + sha256 + size), for its self-update.
    fn host_files_manifest(&self) -> Option<Value> {
        let files: Vec<Value> = HOST_FILES.iter().filter_map(|f| std::fs::read(self.root.join(f)).ok().map(|b| json!({ "name": f, "sha256": pgm_core_internal::crypto::sha256_hex(&b), "size": b.len() }))).collect();
        let (v, c) = self.release();
        if files.is_empty() { None } else { Some(json!({ "version": v, "commit": c, "files": files })) }
    }

    // ---------- Settings → My AI hosts ----------
    fn pub_host(&self, r: &Value) -> Value {
        let id = r["id"].as_str().unwrap_or("");
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let n = live.get(id);
        let providers: Vec<Value> = n.and_then(|n| n.hello.as_ref().map(|h| h["providers"].as_array().cloned().unwrap_or_default().into_iter().map(|p| { let mut x = p.clone(); x["allowedUsers"] = Self::allowed(r, &p); x["declared"] = p["allowedUsers"].clone(); x["scopes"] = if r["scopes"][p["id"].as_str().unwrap_or("")].is_object() { r["scopes"][p["id"].as_str().unwrap_or("")].clone() } else { json!({}) }; x["usage"] = self.usage_of(id, n, &p); let (c, until) = self.cap_of(id, n, &p); x["capped"] = json!(c); x["cappedUntil"] = json!(until); x }).collect())).unwrap_or_default();
        let connected = n.map(|n| n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms).unwrap_or(false);
        let paused = n.map(Self::paused_by_hub).unwrap_or(false);
        let online = connected && !paused;
        json!({ "id": id, "nickname": r["nickname"], "user": r["user"], "paused": paused, "connected": connected, "legacy": r["legacy"].as_bool().unwrap_or(false), "createdAt": r["createdAt"], "online": online,
            "seen": n.filter(|n| n.seen > 0).map(|n| json!(pgm_core_internal::time::iso_at(n.seen as i64))).unwrap_or(Value::Null), "kid": n.map(|n| n.kid.clone()).unwrap_or_default(),
            "version": n.and_then(|n| n.hello.as_ref().map(|h| h["version"].clone())).unwrap_or(json!("")), "os": n.and_then(|n| n.hello.as_ref().map(|h| h["os"].clone())).unwrap_or(json!("")),
            "engines": n.and_then(|n| n.hello.as_ref().map(|h| h["engines"].clone())).unwrap_or(json!([])), "providers": providers })
    }
    pub fn mine(&self, user: &str) -> Vec<Value> { let recs: Vec<Value> = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].as_object().map(|m| m.values().filter(|r| r["user"] == json!(user)).cloned().collect()).unwrap_or_default(); recs.iter().map(|r| self.pub_host(r)).collect() }
    pub fn all(&self) -> Vec<Value> { self.all_records().iter().map(|r| self.pub_host(r)).collect() }
    fn new_nickname(&self) -> String {
        let pick = |n: usize| { let b = pgm_core_internal::crypto::random_bytes(4); (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) % n };
        let taken: HashSet<String> = self.all_records().iter().filter_map(|r| r["nickname"].as_str().map(String::from)).collect();
        for _ in 0..200 { let n = format!("{}-{}", ADJ[pick(ADJ.len())], ANIMAL[pick(ANIMAL.len())]); if !taken.contains(&n) { return n; } }
        format!("{}-{}-{}", ADJ[pick(ADJ.len())], ANIMAL[pick(ANIMAL.len())], pick(100))
    }
    fn check_nick(&self, nick: &str) -> Result<String, String> {
        let n = nick.trim().to_lowercase();
        let parts: Vec<&str> = n.split('-').collect();
        let ok = (2..=4).contains(&parts.len()) && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())) && n.len() <= 40;
        if !ok { return Err("A nickname is two to four lowercase words joined by dashes, like silly-panda.".into()); }
        if self.all_records().iter().any(|r| r["nickname"] == json!(n)) { return Err("That nickname is taken.".into()); }
        Ok(n)
    }
    pub fn create(&self, user: &str, nickname: &str) -> Result<Value, String> {
        if user.is_empty() || user.len() > 32 || !user.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') { return Err("Unknown account.".into()); }
        if self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].as_object().map(|m| m.values().filter(|r| r["user"] == json!(user)).count()).unwrap_or(0) >= 10 { return Err("Ten hosts per person is plenty; remove one first.".into()); }
        let nick = if nickname.is_empty() { self.new_nickname() } else { self.check_nick(nickname)? };
        let id = uuid4();
        let r = json!({ "id": id, "user": user, "nickname": nick, "secret": pgm_core_internal::crypto::random_hex(24), "createdAt": pgm_core_internal::time::iso_now(), "share": {} });
        self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"][&id] = r.clone(); self.save();
        Ok(self.pub_host(&r))
    }
    pub fn rename(&self, user: &str, id: &str, nick: &str) -> Result<Value, String> {
        let owned = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].get(id).map(|r| r["user"] == json!(user)).unwrap_or(false);
        if !owned { return Err("Not your host.".into()); }
        let n = self.check_nick(nick)?;
        self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"][id]["nickname"] = json!(n); self.save();
        Ok(self.pub_host(&self.record(id).unwrap()))
    }
    pub fn remove(&self, user: &str, id: &str) -> Result<(), String> {
        let owned = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].get(id).map(|r| r["user"] == json!(user)).unwrap_or(false);
        if !owned { return Err("Not your host.".into()); }
        if let Some(m) = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].as_object_mut() { m.remove(id); }
        self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(id); self.save();
        Ok(())
    }
    pub fn set_share(&self, user: &str, id: &str, pid: &str, users: &Value) -> Result<Value, String> {
        let owned = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].get(id).map(|r| r["user"] == json!(user)).unwrap_or(false);
        if !owned { return Err("Not your host.".into()); }
        if pid.is_empty() || pid.len() > 32 || !pid.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') { return Err("Unknown provider.".into()); }
        { let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner()); let r = &mut db["hosts"][id]; if !r["share"].is_object() { r["share"] = json!({}); }
          if users.is_null() { if let Some(m) = r["share"].as_object_mut() { m.remove(pid); } } else { r["share"][pid] = json!(clean_users(users)); } }
        self.save();
        Ok(self.pub_host(&self.record(id).unwrap()))
    }
    /// The owner picks what one person (or "@group") may use of a provider: `models` ("*" or some of the provider's
    /// models) and `uses` ("*" or some of work / api / unity). Both "*" = back to everything (the entry goes). Kept on
    /// the hub next to `share`, never sent to the host.
    pub fn set_scope(&self, user: &str, id: &str, pid: &str, who: &str, models: &Value, uses: &Value) -> Result<Value, String> {
        let owned = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].get(id).map(|r| r["user"] == json!(user)).unwrap_or(false);
        if !owned { return Err("Not your host.".into()); }
        if pid.is_empty() || pid.len() > 32 || !pid.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') { return Err("Unknown provider.".into()); }
        let who = who.trim().to_lowercase();
        if who != "@group" && clean_users(&json!([who])).is_empty() { return Err("Say which person (a user name) or @group.".into()); }
        if who == user { return Err("You are the owner: you always have everything.".into()); }
        let offered: Vec<String> = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(id).and_then(|n| n.hello.as_ref().and_then(|h| h["providers"].as_array().and_then(|a| a.iter().find(|p| p["id"] == json!(pid)).cloned())))
            .ok_or("That host has not announced this provider (it must be connected to pick models).")?["models"].as_array().map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect()).unwrap_or_default();
        let models = match models { Value::Null => Scope::default().models, Value::String(s) if s == "*" => None, Value::Array(a) => {
            let l: Vec<String> = a.iter().filter_map(|m| m.as_str().map(String::from)).collect();
            if l.len() != a.len() { return Err("models must be names.".into()); }
            if let Some(bad) = l.iter().find(|m| !offered.contains(m)) { return Err(format!("{bad} is not a model of this provider (it offers: {}).", offered.join(", "))); }
            // every model ticked = all, so a model the host adds later comes with it
            if offered.iter().all(|m| l.contains(m)) { None } else { let mut u: Vec<String> = vec![]; for m in &offered { if l.contains(m) { u.push(m.clone()); } } Some(u) }
        } _ => return Err("models is \"*\" or a list of the provider's models.".into()) };
        let uses = match uses { Value::Null => None, Value::String(s) if s == "*" => None, Value::Array(a) => {
            let l: Vec<String> = a.iter().filter_map(|m| m.as_str().map(String::from)).collect();
            if l.len() != a.len() || l.iter().any(|u| !USES.contains(&u.as_str())) { return Err("uses are work, api and unity.".into()); }
            if USES.iter().all(|u| l.iter().any(|x| x == u)) { None } else { Some(USES.iter().filter(|u| l.iter().any(|x| x == *u)).map(|u| u.to_string()).collect()) }
        } _ => return Err("uses is \"*\" or a list of work, api, unity.".into()) };
        let sc = Scope { models, uses };
        { let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner()); let r = &mut db["hosts"][id]; if !r["scopes"].is_object() { r["scopes"] = json!({}); }
          if !r["scopes"][pid].is_object() { r["scopes"][pid] = json!({}); }
          if !sc.restricted() { if let Some(m) = r["scopes"][pid].as_object_mut() { m.remove(&who); } }
          else if r["scopes"][pid].as_object().map(|m| m.len() >= 50 && !m.contains_key(&who)).unwrap_or(false) { return Err("Fifty scopes per provider is plenty.".into()); }
          else { r["scopes"][pid][&who] = sc.to_value(); }
          if r["scopes"][pid].as_object().map(|m| m.is_empty()).unwrap_or(false) { if let Some(m) = r["scopes"].as_object_mut() { m.remove(pid); } } }
        self.save();
        Ok(self.pub_host(&self.record(id).unwrap()))
    }
    /// The host program's config for one of the person's hosts (what the AI Host download carries).
    pub fn config_for(&self, user: &str, id: &str, hub: &str, models: Vec<String>) -> Result<Value, String> {
        let r = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].get(id).cloned().filter(|r| r["user"] == json!(user)).ok_or("Not your host.")?;
        let nick = r["nickname"].as_str().unwrap_or("");
        Ok(json!({ "_": format!("VibeRoller AI Host \"{nick}\" of {user}: made in Settings → My AI hosts. Keep this file private: the token is this host's key to the hub."), "hub": hub, "id": r["id"], "nickname": nick, "user": user, "token": r["secret"],
            "providers": [{ "id": "claude", "kind": "claude-relay", "label": format!("{user}'s Claude"), "models": models, "max5h": 90, "max7d": 90, "shareWithGroup": true, "allowedUsers": [] },
                { "id": "codex", "kind": "codex-relay", "label": format!("{user}'s ChatGPT / Codex"), "models": ["gpt-5.5"], "shareWithGroup": true, "allowedUsers": [] }],
            "engines": { "ollama": "http://127.0.0.1:11434", "image": "http://127.0.0.1:5124", "voice": "http://127.0.0.1:5123", "speech": "http://127.0.0.1:5125" },
            "autoUpdate": true, "update": { "quietMinutes": 3, "checkMinutes": 5 }, "reconnect": { "maxDelaySeconds": 5 }, "security": { "engineGuard": true, "allowInsecureHub": false } }))
    }
    pub fn enabled(&self) -> bool { !self.all_records().is_empty() }

    // ---------- monitor and control from the hub page (5.12.0; cap `control`) ----------
    /// The person's own hosts as the monitoring panel shows them: the latest status the host reported, online / since,
    /// the usage windows, and who is using it now (the hub knows its jobs in flight: the kind, and for a Work run the
    /// person and project; never what was asked).
    pub fn status_for(&self, user: &str) -> Vec<Value> {
        let recs: Vec<Value> = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].as_object().map(|m| m.values().filter(|r| r["user"] == json!(user) && !user.is_empty()).cloned().collect()).unwrap_or_default();
        let pend: Vec<(String, Value)> = self.pending.lock().unwrap_or_else(|e| e.into_inner()).values().map(|p| (p.host.clone(), use_of(&p.kind, &p.account, &p.display, &p.project, p.started, p.picked))).collect();
        let was = self.was_up.lock().unwrap_or_else(|e| e.into_inner()).clone();
        recs.iter().map(|r| {
            let id = r["id"].as_str().unwrap_or("");
            let base = self.pub_host(r);
            let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let n = live.get(id);
            let caps: Vec<Value> = n.and_then(|n| n.hello.as_ref().and_then(|h| h["caps"].as_array().cloned())).unwrap_or_default();
            let fresh = n.map(|n| now_ms().saturating_sub(n.status_at) < 60_000).unwrap_or(false);
            json!({ "id": id, "nickname": r["nickname"], "online": base["online"], "paused": base["paused"], "since": was.get(id).map(|(_, s)| json!(pgm_core_internal::time::iso_at(*s as i64))).unwrap_or(Value::Null),
                "seen": base["seen"], "version": base["version"], "os": base["os"], "caps": caps, "control": can_control(&caps), "sessionLimits": n.and_then(limits_of), // (from the entry held here: session_limits() would take the same lock again)
                "status": n.map(|n| n.status.clone()).filter(|s| s.is_object()).unwrap_or(Value::Null), "statusAgeMs": n.filter(|n| n.status_at > 0).map(|n| now_ms().saturating_sub(n.status_at)).unwrap_or(0), "statusFresh": fresh,
                "queued": n.map(|n| n.queue.len()).unwrap_or(0), "inUse": pend.iter().filter(|(h, _)| h == id).map(|(_, v)| v.clone()).collect::<Vec<_>>(),
                "providers": base["providers"].as_array().map(|a| a.iter().map(|p| json!({ "id": p["id"], "label": p["label"], "kind": p["kind"], "usage": p["usage"], "max5h": p["max5h"], "max7d": p["max7d"], "capped": p["capped"] })).collect::<Vec<_>>()).unwrap_or_default() })
        }).collect()
    }
    /// Asks one of the person's own hosts to pause / resume / restart / update / send its log. Owner only (an admin is
    /// not the owner), only for a host that announced `control`, and only while it is connected.
    /// The Work-session limits a host set (T-286): what its owner set from the page, else what its hello said; None = the
    /// host says nothing (an older host): the hub's defaults apply.
    pub fn session_limits(&self, id: &str) -> Option<Value> {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.get(id).and_then(limits_of)
    }
    pub async fn control(self: &Arc<Self>, user: &str, id: &str, action: &str, extra: &Value) -> Result<Value, HostErr> {
        let r = self.db.lock().unwrap_or_else(|e| e.into_inner())["hosts"].get(id).cloned().filter(|r| !user.is_empty() && r["user"] == json!(user)).ok_or_else(|| herr(403, "Not your host: only the person who owns an AI host can control it (admins too)."))?;
        let (caps, connected) = { let live = self.live.lock().unwrap_or_else(|e| e.into_inner()); let n = live.get(id); (n.and_then(|n| n.hello.as_ref().and_then(|h| h["caps"].as_array().cloned())).unwrap_or_default(), n.map(|n| n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms).unwrap_or(false)) };
        check_control(&caps, action).map_err(|(s, m)| herr(s, m))?;
        let nick = r["nickname"].as_str().unwrap_or(id).to_string();
        if !connected { return Err(herr(503, format!("{nick} is offline right now: it cannot be controlled from here until it connects."))); }
        let mut body = json!({ "action": action }); if action == "limits" { body["limits"] = extra.clone(); }
        let body = body.to_string().into_bytes();
        let ans = tokio::time::timeout(Duration::from_secs(20), self.relay(id, "control", "/control", "POST", vec![("content-type".into(), "application/json".into())], body, "control", false)).await
            .map_err(|_| herr(504, format!("{nick} did not answer in time.")))??;
        let status = ans.status;
        let out: Value = serde_json::from_slice(&ans.bytes().await).unwrap_or(json!({}));
        if status >= 400 || out["ok"] != json!(true) { return Err(herr(if status >= 400 { status } else { 502 }, out["result"].as_str().or(out["error"]["message"].as_str()).unwrap_or("The host refused.").to_string())); }
        // pause / resume are visible at once; the host's own report follows
        if action == "pause" || action == "resume" { if let Some(n) = self.live.lock().unwrap_or_else(|e| e.into_inner()).get_mut(id) { if !n.status.is_object() { n.status = json!({}); } n.status["paused"] = json!(action == "pause"); n.status["pausedBy"] = json!(if action == "pause" { "hub" } else { "" }); n.status_at = now_ms(); } }
        // what its owner set also goes into the hello kept for a hub restart: a host that keeps its session does not say hello
        // again, so without this the hub would go back to the limits of that host's last hello (found by T-290)
        if action == "limits" && out["limits"].is_object() {
            if let Some(n) = self.live.lock().unwrap_or_else(|e| e.into_inner()).get_mut(id) { n.limits = Some(out["limits"].clone()); if let Some(h) = n.hello.as_mut().and_then(Value::as_object_mut) { h.insert("sessionLimits".into(), clean_limits(&out["limits"])); } }
            self.save_sessions();
        }
        println!("[ai-host] {} ({user}) asked {nick} to {action} from the page", id);
        self.changed();
        Ok(json!({ "ok": true, "result": out["result"], "lines": out["lines"], "limits": out["limits"] }))
    }
}
/// A host's announced session limits, kept only in their known shape (numbers 1 to 100, real names); None = none sent.
fn clean_limits(v: &Value) -> Value {
    if !v.is_object() { return Value::Null; }
    let n = |x: &Value| x.as_u64().filter(|n| (1..=100).contains(n));
    let name = |k: &str| !k.is_empty() && k.len() <= 32 && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let map = |m: &Value| -> Map<String, Value> { m.as_object().map(|o| o.iter().filter(|(k, _)| name(k)).filter_map(|(k, x)| n(x).map(|x| (k.clone(), json!(x)))).collect()).unwrap_or_default() };
    json!({ "perUser": n(&v["perUser"]).unwrap_or(5), "perProvider": n(&v["perProvider"]).unwrap_or(3), "providers": map(&v["providers"]), "exceptions": map(&v["exceptions"]), "ownerBypass": v["ownerBypass"] != json!(false) })
}
/// A host's session limits from its live entry: what its owner set from the page, else what its hello said.
fn limits_of(n: &Live) -> Option<Value> { n.limits.clone().or_else(|| n.hello.as_ref().and_then(|h| h.get("sessionLimits").cloned())).filter(Value::is_object) }
/// Does a host's hello say it takes control jobs?
pub fn can_control(caps: &[Value]) -> bool { caps.iter().any(|c| c == pgm_net_internal::hostlink::CAP_CONTROL) }
/// May this action go to a host with these caps? An older host gets a clear message, not a job it cannot read.
pub fn check_control(caps: &[Value], action: &str) -> Result<(), (u16, String)> {
    if !["pause", "resume", "restart", "update", "logs", "limits"].contains(&action) { return Err((400, format!("Unknown action {action:?}: pause, resume, restart, update, logs or limits."))); }
    if !can_control(caps) { return Err((400, "This host is older than 5.13: update it to control it from here.".into())); }
    Ok(())
}
/// One job in flight as the owner sees it: what kind, and for a Work run who and which project (never content).
fn use_of(kind: &str, account: &str, display: &str, project: &str, started: u64, picked: bool) -> Value {
    let (k, what) = match kind { "work" => ("work", "a Work run"), "provider" => ("answer", "an answer (chat / API)"), "engine" => ("engine", "one of Unity's engines"), "pc" => ("computer", "a computer tool"), "control" => ("control", "a control"), o => (o, "a job") };
    json!({ "kind": k, "what": what, "person": if kind == "work" { if display.is_empty() { account } else { display } } else { "" }, "project": project, "since": started, "running": picked })
}
/// The status a host reported, cleaned: known keys only, bounded text, numbers as numbers (a hub ignores what it does not know).
pub fn clean_status(b: &Value) -> Value {
    let text = |k: &str, n: usize| json!(b[k].as_str().unwrap_or("").chars().filter(|c| !c.is_control()).take(n).collect::<String>());
    let num = |k: &str| json!(b[k].as_u64().unwrap_or(0));
    let window = |w: &Value| if w.is_object() { json!({ "utilization": w["utilization"].as_f64().map(|x| x.clamp(0.0, 1.0)), "resetsAt": w["resetsAt"].as_f64() }) } else { Value::Null };
    let usage: Map<String, Value> = b["usage"].as_object().map(|m| m.iter().take(20).map(|(k, v)| (k.chars().take(32).collect::<String>(), json!({ "five_hour": window(&v["five_hour"]), "seven_day": window(&v["seven_day"]) }))).collect()).unwrap_or_default();
    let logins: Map<String, Value> = b["logins"].as_object().map(|m| m.iter().take(20).map(|(k, v)| (k.chars().take(32).collect::<String>(), match v.as_str() { Some(s) => json!(s.chars().filter(|c| !c.is_control()).take(200).collect::<String>()), None => Value::Null })).collect()).unwrap_or_default();
    json!({ "version": text("version", 20), "target": text("target", 40), "startedAt": num("startedAt"), "paused": b["paused"] == json!(true), "pausedBy": text("pausedBy", 10),
        "active": num("active"), "runs": num("runs"), "lastJobAt": num("lastJobAt"), "lastUpdate": text("lastUpdate", 200), "lastUpdateAt": num("lastUpdateAt"), "pending": text("pending", 100),
        "usage": usage, "logins": logins })
}
/// What the checkout above `root` runs: (version, commit) (pgm-dist-internal's version stamp).
pub fn release_now(root: &std::path::Path) -> (String, String) { pgm_dist_internal::version::release_of(&root.join("..")) }
/// A host's status code, when it is one an HTTP answer can carry (else 502): a host never decides what breaks here.
fn host_status(v: &Value) -> u16 { v.as_u64().filter(|s| (100..=599).contains(s)).map(|s| s as u16).unwrap_or(502) }
/// The headers of a host's answer that are passed on: known names only, and only ones that are valid HTTP headers.
fn pick_headers(v: &Value) -> Vec<(String, String)> {
    let ok = |k: &str| { let k = k.to_lowercase(); matches!(k.as_str(), "content-type" | "request-id" | "retry-after") || k.starts_with("anthropic-ratelimit-") || k.starts_with("x-codex-") || k.starts_with("openai-") };
    v.as_object().map(|m| m.iter().filter(|(k, v)| ok(k) && v.as_str().map(|s| hyper::header::HeaderValue::from_str(s).is_ok()).unwrap_or(false) && hyper::header::HeaderName::from_bytes(k.to_lowercase().as_bytes()).is_ok()).map(|(k, v)| (k.to_lowercase(), v.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default()
}
#[cfg(test)]
mod tests {
    #[test]
    fn a_hello_lists_the_brain_among_its_models() {
        let h = serde_json::json!({ "engines": ["ollama"], "providers": [{ "id": "claude", "models": ["claude-opus-5-5"] }, { "id": "ollama", "kind": "openai", "models": ["llama3.1:8b", "unity-brain:latest"] }] });
        assert!(super::hello_has_model(&h, "unity-brain")); assert!(super::hello_has_model(&h, "unity-brain:latest")); assert!(!super::hello_has_model(&h, "qwen3"));
        assert!(!super::hello_has_model(&serde_json::json!({ "engines": ["ollama"] }), "unity-brain"), "an older host's hello: no models known");
    }
    use super::*;
    #[test]
    fn a_scope_limits_models_and_uses_and_defaults_to_everything() {
        let scopes = json!({ "t-user": { "models": ["m1"], "uses": ["api"] }, "@group": { "models": ["m1", "m2"], "uses": "*" }, "partial": { "uses": ["work"] } });
        // nobody listed, or a person outside the group: everything (today's sharing)
        let all = scope_of(&scopes, "stranger", false);
        assert_eq!(all, Scope::default()); assert!(!all.restricted());
        assert!(check_scope(&all, "gee", "Claude", "unity", "anything").is_ok());
        // a model outside the scope is refused, the error names the owner and what is allowed
        let own = scope_of(&scopes, "t-user", true);
        assert!(check_scope(&own, "gee", "Gee's Claude", "api", "m1").is_ok());
        let e = check_scope(&own, "gee", "Gee's Claude", "api", "m2").unwrap_err();
        assert!(e.contains("gee") && e.contains("m2") && e.contains("m1"), "{e}");
        // an empty model means "whichever the provider picks": allowed
        assert!(check_scope(&own, "gee", "Gee's Claude", "api", "").is_ok());
        // a use outside the scope is refused
        let e = check_scope(&own, "gee", "Gee's Claude", "work", "m1").unwrap_err();
        assert!(e.contains("gee") && e.contains("Work runs") && e.contains("the API"), "{e}");
        assert!(check_scope(&own, "gee", "Gee's Claude", "unity", "m1").is_err());
        // "@group" applies to members of the group; the person's own entry wins over it
        let member = scope_of(&scopes, "somebody", true);
        assert_eq!(member.models, Some(vec!["m1".to_string(), "m2".to_string()])); assert!(member.uses.is_none());
        assert!(check_scope(&member, "gee", "C", "unity", "m2").is_ok() && check_scope(&member, "gee", "C", "work", "m3").is_err());
        assert_ne!(own, member); assert_eq!(own.uses, Some(vec!["api".to_string()]));
        assert_eq!(scope_of(&scopes, "somebody", false), Scope::default()); // not in the group: the group's scope does not apply
        // a field left out means all of it
        let part = scope_of(&scopes, "partial", true);
        assert!(part.models.is_none() && check_scope(&part, "gee", "C", "work", "x").is_ok() && check_scope(&part, "gee", "C", "api", "x").is_err());
        // no model switched on at all is refused whatever is asked
        let none = Scope::from_value(&json!({ "models": [], "uses": "*" }));
        assert!(check_scope(&none, "gee", "C", "work", "").is_err());
        // unknown uses in a stored entry are dropped, not trusted
        assert_eq!(Scope::from_value(&json!({ "uses": ["api", "root"] })).uses, Some(vec!["api".to_string()]));
    }
    #[test]
    fn a_scope_narrows_the_view_a_person_gets() {
        let mut v = json!({ "id": "h:x/claude", "models": ["m1", "m2", "m3"] });
        assert!(AiHosts::scope_view(&mut v, &Scope::default())); assert_eq!(v["models"], json!(["m1", "m2", "m3"])); assert!(v.get("scope").is_none());
        let sc = Scope { models: Some(vec!["m3".into(), "m1".into()]), uses: Some(vec!["work".into()]) };
        assert!(AiHosts::scope_view(&mut v, &sc));
        assert_eq!(v["models"], json!(["m1", "m3"])); // the provider's own order
        assert_eq!(v["scope"], json!({ "models": ["m3", "m1"], "uses": ["work"] }));
        let mut gone = json!({ "models": ["m1"] });
        assert!(!AiHosts::scope_view(&mut gone, &Scope { models: Some(vec!["m9".into()]), uses: None }));
    }
    #[test]
    fn a_control_goes_only_to_a_host_that_announced_it() {
        let new = vec![json!("relay"), json!("control")]; let old = vec![json!("relay"), json!("update-bin")];
        for a in ["pause", "resume", "restart", "update", "logs"] { assert!(check_control(&new, a).is_ok()); }
        let (s, m) = check_control(&old, "pause").unwrap_err();
        assert_eq!(s, 400); assert!(m.contains("older than 5.13") && m.contains("update it"), "{m}");
        assert!(check_control(&[], "pause").is_err());
        assert_eq!(check_control(&new, "format-disk").unwrap_err().0, 400);
        assert!(!can_control(&[json!("controls")]) && can_control(&new));
    }
    #[test]
    fn a_reported_status_is_cleaned_and_a_use_shows_no_content() {
        let st = clean_status(&json!({ "version": "5.12.0", "paused": true, "pausedBy": "hub", "active": 2, "runs": 1, "lastUpdate": "updated\nto 5.12.0", "evil": "x", "message": "secret text",
            "usage": { "claude": { "five_hour": { "utilization": 7.5, "resetsAt": 1.0 }, "seven_day": { "utilization": 0.25 } } }, "logins": { "claude": null, "codex": "no login" } }));
        assert_eq!(st["paused"], json!(true)); assert_eq!(st["active"], json!(2)); assert!(st.get("evil").is_none() && st.get("message").is_none());
        assert_eq!(st["lastUpdate"], json!("updatedto 5.12.0")); // control characters dropped
        assert_eq!(st["usage"]["claude"]["five_hour"]["utilization"], json!(1.0)); // clamped to a fraction
        assert_eq!(st["usage"]["claude"]["seven_day"]["utilization"], json!(0.25)); assert!(st["logins"]["claude"].is_null() && st["logins"]["codex"] == json!("no login"));
        assert!(clean_status(&json!(null))["usage"].as_object().unwrap().is_empty());
        let u = use_of("work", "gee", "Gee", "demo-app", 5, true);
        assert_eq!(u["person"], json!("Gee")); assert_eq!(u["project"], json!("demo-app")); assert_eq!(u["kind"], json!("work"));
        assert_eq!(use_of("provider", "someone", "Someone", "", 5, true)["person"], json!(""));
    }
    #[test]
    fn a_jcode_provider_is_listed_only_for_a_host_that_announced_work_jcode() {
        let list = json!([{ "id": "claude", "kind": "claude-relay", "models": ["m"] }, { "id": "jcode", "kind": "jcode", "label": "J", "models": ["m1"] }]);
        let ids = |caps: &[&str]| AiHosts::clean_providers(&list, &caps.iter().map(|c| c.to_string()).collect::<Vec<_>>()).into_iter().map(|p| p["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(ids(&["relay", "update", "update-bin"]), ["claude"]);
        assert_eq!(ids(&[]), ["claude"]);
        assert_eq!(ids(&["relay", "work-jcode"]), ["claude", "jcode"]);
        assert_eq!(AiHosts::hub_kind("jcode"), "host-jcode"); assert_eq!(AiHosts::hub_kind("claude-relay"), "host-claude");
    }
}
fn uuid4() -> String { let b = pgm_core_internal::crypto::random_bytes(16); let mut b: [u8; 16] = b.try_into().expect("16"); b[6] = (b[6] & 0x0f) | 0x40; b[8] = (b[8] & 0x3f) | 0x80; let h = hex::encode(b); format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]) }
