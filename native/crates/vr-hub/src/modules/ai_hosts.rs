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

use vr_core::crypto::{Envelope, HostKey};
use vr_core::now_ms;

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
struct Pending { kind: String, host: String, waiting: Option<Waiting>, token: String, account: String, display: String, provider: String, last: u64, picked: bool }

#[derive(Default)]
struct Live { seen: u64, queue: Vec<Value>, waiters: usize, cancel: HashSet<String>, hello: Option<Value>, key: Option<HostKey>, kid: String, usage: Map<String, Value>, relay_ok: Map<String, Value>, notify: Arc<Notify> }

pub struct AiHosts {
    root: PathBuf,
    cfg: super::Config,
    db: Mutex<Value>,
    legacy: Vec<Value>,
    live: Mutex<HashMap<String, Live>>,
    pending: Mutex<HashMap<String, Pending>>,
    was_up: Mutex<HashMap<String, (bool, u64)>>,
    offline_ms: u64,
    release_at: Mutex<(u64, String, String)>,
    pub usage: Arc<super::usage::Usage>,
    /// something people see changed (a hello, a host up / down, its usage, the registry): the JavaScript hub hears it
    pub dirty: Notify,
}
fn same_secret(a: &str, b: &str) -> bool { use subtle::ConstantTimeEq; let (x, y) = (vr_core::crypto::sha256(a.as_bytes()), vr_core::crypto::sha256(b.as_bytes())); bool::from(x.ct_eq(&y)) }
fn clean_users(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')).take(50).collect()).unwrap_or_default() }
fn job_id() -> String { use base64::Engine; base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vr_core::crypto::random_bytes(9)) }

impl AiHosts {
    pub fn new(root: &std::path::Path, cfg: &super::Config, usage: Arc<super::usage::Usage>) -> Arc<AiHosts> {
        let mut db: Value = std::fs::read_to_string(root.join("ai-hosts.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({ "hosts": {} }));
        if !db["hosts"].is_object() { db["hosts"] = json!({}); }
        let legacy: Vec<Value> = cfg.get("aiNodes").and_then(Value::as_object).map(|m| m.iter().filter(|(n, s)| !n.is_empty() && n.len() <= 40 && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && s.as_str().map(|s| s.len() >= 24).unwrap_or(false))
            .map(|(n, s)| json!({ "id": n, "user": "", "nickname": n, "secret": s, "legacy": true, "createdAt": Value::Null, "share": {} })).collect()).unwrap_or_default();
        let (version, commit) = release_now(root);
        let h = Arc::new(AiHosts { root: root.to_path_buf(), cfg: cfg.clone(), db: Mutex::new(db), legacy, live: Mutex::default(), pending: Mutex::default(), was_up: Mutex::default(),
            offline_ms: cfg.get("aiNodeOfflineMs").and_then(Value::as_u64).unwrap_or(60000).max(1000), release_at: Mutex::new((now_ms(), version, commit)), usage, dirty: Notify::new() });
        h.restore_sessions();
        h
    }
    fn save(&self) { let db = self.db.lock().unwrap().clone(); let f = self.root.join("ai-hosts.json"); let tmp = f.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(&db).unwrap_or_default()).is_ok() { vr_core::settings::private(&tmp); let _ = std::fs::rename(&tmp, &f); } self.changed(); }
    pub fn record(&self, id: &str) -> Option<Value> { self.db.lock().unwrap()["hosts"].get(id).cloned().or_else(|| self.legacy.iter().find(|r| r["id"] == json!(id)).cloned()) }
    fn all_records(&self) -> Vec<Value> { let mut v: Vec<Value> = self.db.lock().unwrap()["hosts"].as_object().map(|m| m.values().cloned().collect()).unwrap_or_default(); v.extend(self.legacy.clone()); v }
    // ---------- sessions, kept across restarts ----------
    fn restore_sessions(&self) {
        let s: Value = std::fs::read_to_string(self.root.join("ai-host-sessions.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        let mut live = self.live.lock().unwrap();
        for (id, v) in s.as_object().cloned().unwrap_or_default() {
            if self.record(&id).is_none() || !v["hello"].is_object() { continue; }
            let Some(key) = v["key"].as_str().and_then(HostKey::from_hex) else { continue };
            let n = live.entry(id).or_default();
            n.kid = key.id(); n.key = Some(key); n.hello = Some(v["hello"].clone()); n.relay_ok = v["relayOk"].as_object().cloned().unwrap_or_default(); n.seen = now_ms();
        }
    }
    fn save_sessions(&self) {
        let o: Map<String, Value> = self.live.lock().unwrap().iter().filter(|(id, n)| n.hello.is_some() && n.key.is_some() && self.record(id).is_some())
            .map(|(id, n)| (id.clone(), json!({ "hello": n.hello, "key": n.key.as_ref().map(|k| k.to_hex()), "relayOk": n.relay_ok }))).collect();
        let f = self.root.join("ai-host-sessions.json"); let tmp = f.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(&Value::Object(o)).unwrap_or_default()).is_ok() { vr_core::settings::private(&tmp); let _ = std::fs::rename(&tmp, &f); }
        self.changed();
    }
    pub fn online(&self, id: &str) -> bool { self.live.lock().unwrap().get(id).map(|n| n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms).unwrap_or(false) }
    pub fn changed(&self) { self.dirty.notify_one(); }
    /// What the JavaScript hub needs to answer without asking (its Work runs, the API, the chat ask these all the time):
    /// each host's state, and what each account may pick. Sent on every change and every 5 s; online is counted there
    /// from seenMs + polling, so it never lags.
    pub fn snapshot(&self, users: &[String]) -> Value {
        let records = self.all_records();
        let mut hosts = Map::new(); let mut by_id = Map::new();
        {
            let live = self.live.lock().unwrap();
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
                let prev = h.was_up.lock().unwrap().get(&id).cloned();
                match prev {
                    None => { if h.live.lock().unwrap().contains_key(&id) { h.was_up.lock().unwrap().insert(id, (up, now_ms())); } }
                    Some((p, since)) if p != up => { println!("[ai-host] {} is {} ({} for {} s)", r["nickname"].as_str().unwrap_or(&id), if up { "back" } else { "OFFLINE" }, if up { "away" } else { "up" }, (now_ms() - since) / 1000); h.was_up.lock().unwrap().insert(id, (up, now_ms())); }
                    _ => {}
                }
            }
            h.changed();
        } });
    }
    // ---------- what hosts offer ----------
    fn owner_of(&self, r: &Value) -> String { let u = r["user"].as_str().unwrap_or(""); if !u.is_empty() { return u.into(); } self.live.lock().unwrap().get(r["id"].as_str().unwrap_or("")).and_then(|n| n.hello.as_ref().and_then(|h| h["user"].as_str().map(String::from))).unwrap_or_default() }
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
    fn hub_kind(k: &str) -> &'static str { match k { "openai" => "host-openai", "codex-relay" => "host-codex", _ => "host-claude" } }
    fn remote_kind(n: &Live, p: &Value) -> String { let k = p["kind"].as_str().unwrap_or(""); let pid = p["id"].as_str().unwrap_or(""); if k == "claude-cli" && n.relay_ok.get(pid) == Some(&json!(false)) { "claude-cli-work".into() } else { k.into() } }
    /// A provider as people see it (the live map is held by the caller).
    fn prov_view_locked(&self, r: &Value, id: &str, n: &Live, p: &Value) -> Value {
        let owner = { let u = r["user"].as_str().unwrap_or(""); if !u.is_empty() { u.to_string() } else { n.hello.as_ref().and_then(|h| h["user"].as_str().map(String::from)).unwrap_or_default() } };
        json!({ "id": format!("h:{id}/{}", p["id"].as_str().unwrap_or("")), "kind": Self::hub_kind(p["kind"].as_str().unwrap_or("")), "remoteKind": Self::remote_kind(n, p), "label": p["label"], "owner": owner, "shared": true, "models": p["models"],
            "hostId": id, "providerId": p["id"], "nickname": r["nickname"], "online": n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms, "max5h": p["max5h"], "max7d": p["max7d"], "usage": self.usage_of(id, n, p), "capped": self.over_cap(id, n, p), "cappedUntil": self.cap_of(id, n, p).1 })
    }
    /// The providers a person may pick from the hosts: "h:<hostId>/<providerId>".
    pub fn providers_for(&self, user: &str) -> Vec<Value> {
        let mut out = vec![];
        let records = self.all_records();
        let live = self.live.lock().unwrap();
        for r in records {
            let id = r["id"].as_str().unwrap_or("");
            let Some(n) = live.get(id) else { continue }; let Some(hello) = &n.hello else { continue };
            for p in hello["providers"].as_array().cloned().unwrap_or_default() { if self.may_use_locked(&r, &p, user, n) { let mut v = self.prov_view_locked(&r, id, n, &p); if let Some(o) = v.as_object_mut() { o.remove("providerId"); } out.push(v); } }
        }
        out
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
        let r = self.record(hid)?; let live = self.live.lock().unwrap(); let n = live.get(hid)?; let hello = n.hello.as_ref()?;
        let p = hello["providers"].as_array()?.iter().find(|x| x["id"] == json!(prov))?.clone();
        if !user.is_empty() && !self.may_use_locked(&r, &p, user, n) { return None; }
        let mut v = self.prov_view_locked(&r, hid, n, &p); v["id"] = json!(pid); Some(v)
    }
    /// The person's own AI host that offers their computer (the native app's "pc"), when it is online.
    pub fn pc_host_for(&self, user: &str) -> Option<String> {
        if user.is_empty() { return None; }
        for r in self.all_records() {
            let id = r["id"].as_str().unwrap_or("").to_string();
            let pc = self.live.lock().unwrap().get(&id).and_then(|n| n.hello.as_ref().map(|h| h["pc"] == json!(true))).unwrap_or(false);
            if pc && self.owner_of(&r) == user && self.online(&id) { return Some(id); }
        }
        None
    }
    pub fn status(&self) -> Vec<Value> {
        let live = self.live.lock().unwrap(); let was = self.was_up.lock().unwrap();
        self.all_records().iter().map(|r| {
            let id = r["id"].as_str().unwrap_or(""); let n = live.get(id);
            json!({ "id": id, "name": r["nickname"], "nickname": r["nickname"], "user": r["user"], "online": n.map(|n| n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms).unwrap_or(false), "polling": n.map(|n| n.waiters > 0).unwrap_or(false),
                "seen": n.filter(|n| n.seen > 0).map(|n| json!(super::auth::iso_at(n.seen as i64))).unwrap_or(Value::Null), "queued": n.map(|n| n.queue.len()).unwrap_or(0),
                "engines": n.and_then(|n| n.hello.as_ref().map(|h| h["engines"].clone())).unwrap_or(json!([])), "providers": n.and_then(|n| n.hello.as_ref().map(|h| json!(h["providers"].as_array().map(|a| a.iter().map(|p| p["id"].clone()).collect::<Vec<_>>()).unwrap_or_default()))).unwrap_or(json!([])),
                "kid": n.map(|n| n.kid.clone()).unwrap_or_default(), "version": n.and_then(|n| n.hello.as_ref().map(|h| h["version"].clone())).unwrap_or(json!("")), "since": was.get(id).map(|(_, s)| json!(super::auth::iso_at(*s as i64))).unwrap_or(Value::Null) })
        }).collect()
    }
    fn pick_engine_host(&self, engine: &str) -> Option<String> {
        if let Some(want) = self.cfg.get("assistant").and_then(|a| a["node"].as_str()).filter(|s| !s.is_empty()) {
            let ok = self.online(want) && self.live.lock().unwrap().get(want).map(|n| n.hello.as_ref().map(|h| h["engines"].as_array().map(|a| a.iter().any(|e| e == engine)).unwrap_or(false)).unwrap_or(true)).unwrap_or(false);
            return if ok { Some(want.to_string()) } else { None };
        }
        let ids: Vec<String> = self.live.lock().unwrap().iter().filter(|(_, n)| n.hello.as_ref().map(|h| h["engines"].as_array().map(|a| a.iter().any(|e| e == engine)).unwrap_or(false)).unwrap_or(true)).map(|(id, _)| id.clone()).collect();
        // Unity's engines see everyone's prompts, voice and pictures: only a host from the hub's config or one of a hub
        // admin's lends them, never just anyone's
        ids.into_iter().find(|id| self.online(id) && self.record(id).map(|r| r["legacy"] == json!(true) || self.owner_is_admin(&r)).unwrap_or(false))
    }

    fn owner_is_admin(&self, r: &Value) -> bool {
        let u = r["user"].as_str().unwrap_or(""); if u.is_empty() || !super::auth::valid_username(u) { return false; }
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
            let mut live = self.live.lock().unwrap();
            let n = live.entry(host.to_string()).or_default();
            if n.queue.len() >= MAX_QUEUE { return Err(herr(503, "That AI host is overloaded; try again in a moment.")); }
            n.queue.push(job); n.notify.clone()
        };
        let kind = p.kind.clone();
        self.pending.lock().unwrap().insert(id.clone(), p);
        notify.notify_waiters();
        // not picked up in time: the host is away
        let (h, id2, host2) = (self.clone(), id.clone(), host.to_string());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(PICKUP_MS)).await;
            let still = h.live.lock().unwrap().get(&host2).map(|n| n.queue.iter().any(|j| j["id"] == json!(id2))).unwrap_or(false);
            if still { let nick = h.record(&host2).and_then(|r| r["nickname"].as_str().map(String::from)).unwrap_or(host2.clone()); h.finish(&id2, Some(if kind == "work" { format!("{nick} did not pick the job up: the host is away.") } else { OFFLINE_TEXT.to_string() }), None); }
        });
        Ok(())
    }
    /// Ends a job: its waiter gets the error, the end of its stream, or the run's end.
    fn finish(&self, id: &str, err: Option<String>, done_code: Option<i32>) {
        let Some(p) = self.pending.lock().unwrap().remove(id) else { return };
        if let Some(n) = self.live.lock().unwrap().get_mut(&p.host) { n.queue.retain(|j| j["id"] != json!(id)); n.cancel.remove(id); }
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
        let job = json!({ "id": id, "kind": kind, "engine": engine, "path": path, "method": method, "headers": h, "body": if body.is_empty() { String::new() } else { vr_core::b64(&body) } });
        let (tx, rx) = oneshot::channel();
        self.enqueue(host, job, Pending { kind: kind.into(), host: host.into(), waiting: Some(Waiting::Whole(tx)), token: String::new(), account: String::new(), display: String::new(), provider: String::new(), last: now_ms(), picked: false })?;
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
    pub async fn provider_fetch(self: &Arc<Self>, host: &str, pid: &str, path: &str, method: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<Relayed, HostErr> {
        if !self.online(host) { return Err(herr(503, format!("That AI host ({}) is offline right now.", self.record(host).and_then(|r| r["nickname"].as_str().map(String::from)).unwrap_or(host.into())))); }
        self.relay(host, &format!("provider:{pid}"), path, method, headers, body, "provider", false).await
    }
    /// A model call of this hub's own Claude Code / Codex, carried to the host that adds its login. Usage headers feed its caps.
    pub async fn login_fetch(self: &Arc<Self>, host: &str, pid: &str, path: &str, method: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<Relayed, HostErr> {
        if !self.online(host) { return Err(herr(503, format!("That AI host ({}) is offline right now.", self.record(host).and_then(|r| r["nickname"].as_str().map(String::from)).unwrap_or(host.into())))); }
        let cap = { let live = self.live.lock().unwrap(); live.get(host).and_then(|n| n.hello.as_ref().and_then(|h| h["providers"].as_array().and_then(|a| a.iter().find(|p| p["id"] == json!(pid)).cloned())).map(|p| self.over_cap(host, n, &p))).unwrap_or_default() };
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
        let prev = self.live.lock().unwrap().get(host).and_then(|n| n.usage.get(pid).cloned()).unwrap_or(json!({}));
        self.note_host_usage(host, pid, &json!({ "five_hour": if five.is_null() { prev["five_hour"].clone() } else { five }, "seven_day": if seven.is_null() { prev["seven_day"].clone() } else { seven } }));
    }
    fn note_host_usage(&self, host: &str, pid: &str, u: &Value) {
        let clean = |w: &Value| -> Value { if !w.is_object() { return Value::Null; } let n = w["utilization"].as_f64().or_else(|| w["utilization"].as_str().and_then(|s| s.parse().ok())); json!({ "utilization": n.map(|x| (if x > 1.0 { x / 100.0 } else { x }).clamp(0.0, 1.0)), "resetsAt": w["resetsAt"].as_f64(), "status": w["status"].as_str().unwrap_or("") }) };
        let rec = json!({ "five_hour": clean(&u["five_hour"]), "seven_day": clean(&u["seven_day"]), "at": now_ms() });
        if let Some(n) = self.live.lock().unwrap().get_mut(host) { n.usage.insert(pid.to_string(), rec.clone()); }
        self.usage.note_windows(&format!("h:{host}/{pid}"), &rec);
        self.changed();
    }
    pub async fn pc_tool(self: &Arc<Self>, host: &str, name: &str, args: &Value) -> Result<Value, HostErr> {
        let r = self.relay(host, "pc", &format!("/{name}"), "POST", vec![("content-type".into(), "application/json".into())], args.to_string().into_bytes(), "pc", false).await?;
        let b = r.bytes().await;
        serde_json::from_slice(&b).map_err(|e| herr(502, e.to_string()))
    }
    /// A Work run on a host (2.x hosts and hosts that allow Work): Claude Code runs there; events come back sealed.
    pub fn run_work(self: &Arc<Self>, host: &str, pid: &str, payload: Value) -> Result<(String, String, mpsc::UnboundedReceiver<WorkMsg>), HostErr> {
        let r = self.record(host).ok_or_else(|| herr(503, "That AI host has not connected yet."))?;
        let nick = r["nickname"].as_str().unwrap_or(host).to_string();
        let (key, prov) = { let live = self.live.lock().unwrap(); let n = live.get(host).ok_or_else(|| herr(503, "That AI host has not connected yet."))?; (n.key.clone(), n.hello.as_ref().and_then(|h| h["providers"].as_array().and_then(|a| a.iter().find(|p| p["id"] == json!(pid)).cloned()))) };
        let key = key.ok_or_else(|| herr(503, "That AI host has not connected yet."))?;
        if !self.online(host) { let o = self.owner_of(&r); return Err(herr(503, format!("{nick}{} is offline right now. Pick another AI in the dropdown, or wait for it.", if o.is_empty() { " (AI host)".to_string() } else { format!(" ({o}'s AI host)") }))); }
        let prov = prov.ok_or_else(|| herr(503, "That host no longer offers this provider."))?;
        let cap = { let live = self.live.lock().unwrap(); live.get(host).map(|n| self.over_cap(host, n, &prov)).unwrap_or_default() };
        if !cap.is_empty() { return Err(herr(429, format!("{nick}: {cap}. Pick another AI or wait for the window to reset."))); }
        let id = job_id(); let token = vr_core::crypto::random_hex(24);
        let mut p = payload.clone(); p["providerId"] = json!(pid); p["token"] = json!(token); p["mcpBase"] = json!(format!("{}/node/job/{id}", payload["hubUrl"].as_str().unwrap_or("")));
        let enc = key.seal(&p).map_err(|e| herr(500, e.to_string()))?;
        let (tx, rx) = mpsc::unbounded_channel();
        self.enqueue(host, json!({ "id": id, "kind": "work", "enc": enc }), Pending { kind: "work".into(), host: host.into(), waiting: Some(Waiting::Work(tx)), token: token.clone(), account: payload["account"].as_str().unwrap_or("").into(), display: payload["display"].as_str().unwrap_or("").into(), provider: pid.into(), last: now_ms(), picked: false })?;
        // no word for ten minutes: given up
        let (h, id2) = (self.clone(), id.clone());
        tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(30)).await; let last = h.pending.lock().unwrap().get(&id2).map(|p| p.last); match last { Some(l) if now_ms() - l > IDLE_MS => { h.finish(&id2, Some("No word from the AI host for ten minutes; the run was given up.".into()), None); return; } Some(_) => {} None => return } } });
        Ok((id, token, rx))
    }
    pub fn cancel_work(&self, id: &str) { let host = self.pending.lock().unwrap().get(id).map(|p| p.host.clone()); if let Some(h) = host { let n = { let mut live = self.live.lock().unwrap(); live.get_mut(&h).map(|n| { n.cancel.insert(id.to_string()); n.notify.clone() }) }; if let Some(n) = n { n.notify_waiters(); } } }
    /// A host's Work run calling the team tools: its account, when the token is the run's.
    pub fn job_for(&self, id: &str, token: &str) -> Option<(String, String)> { let p = self.pending.lock().unwrap(); let j = p.get(id)?; if j.kind == "work" && !token.is_empty() && same_secret(token, &j.token) { Some((j.account.clone(), j.display.clone())) } else { None } }

    // ---------- older hosts: can this one relay a login, or does its Claude run Work itself (2.x)? ----------
    fn probe_relays(self: &Arc<Self>, id: String) {
        let h = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let provs: Vec<String> = h.live.lock().unwrap().get(&id).and_then(|n| n.hello.as_ref().map(|hh| hh["providers"].as_array().map(|a| a.iter().filter(|p| p["kind"] == json!("claude-cli") || p["kind"] == json!("claude-relay")).filter_map(|p| p["id"].as_str().map(String::from)).collect()).unwrap_or_default())).unwrap_or_default();
            for pid in provs {
                let Ok(r) = h.relay(&id, &format!("relay:{pid}"), "/v1/__viberoller_probe", "POST", vec![("content-type".into(), "application/json".into())], b"{}".to_vec(), "provider", true).await else { continue };
                let status = r.status; let text = String::from_utf8_lossy(&r.bytes().await).to_lowercase();
                let ok = !(status == 404 && text.contains("does not lend"));
                if let Some(n) = h.live.lock().unwrap().get_mut(&id) { n.relay_ok.insert(pid.clone(), json!(ok)); }
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
    fn clean_providers(list: &Value) -> Vec<Value> {
        let mut out: Vec<Value> = vec![];
        let pct = |v: &Value, d: i64| v.as_f64().filter(|n| *n > 0.0 && *n <= 100.0).map(|n| n.round() as i64).unwrap_or(d);
        for p in list.as_array().cloned().unwrap_or_default().into_iter().take(20) {
            let id = p["id"].as_str().unwrap_or("").trim().to_string(); let kind = p["kind"].as_str().unwrap_or("");
            if id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') || !["claude-cli", "claude-relay", "codex-relay", "anthropic", "openai"].contains(&kind) || out.iter().any(|x| x["id"] == json!(id)) { continue; }
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
        let mut r = self.release_at.lock().unwrap();
        if now_ms() - r.0 > 30_000 { let (v, c) = release_now(&self.root); *r = (now_ms(), v, c); }
        (r.1.clone(), r.2.clone())
    }
    fn beat(&self) -> Value { let (v, c) = self.release(); json!({ "t": now_ms(), "version": v, "commit": c }) }
    /// The /node/* routes: (status, answer) or a raw body for a file.
    pub async fn route(self: &Arc<Self>, method: &hyper::Method, path: &str, query: &str, hs: &hyper::HeaderMap, body: &[u8], native: &super::native_releases::Releases) -> (u16, Result<Value, (Vec<u8>, &'static str)>) {
        let Some(r) = self.who(hs) else { return (401, Ok(json!({ "error": "Unknown AI host." }))) };
        let id = r["id"].as_str().unwrap_or("").to_string();
        let b: Value = serde_json::from_slice(body).unwrap_or(json!({}));
        let notify = { let mut live = self.live.lock().unwrap(); let n = live.entry(id.clone()).or_default(); n.seen = now_ms(); n.notify.clone() };
        let get = *method == hyper::Method::GET; let post = *method == hyper::Method::POST;
        match path {
            "/node/hello" if post => {
                let nonce = b["nonce"].as_str().unwrap_or("").to_string();
                if !(32..=64).contains(&nonce.len()) || !nonce.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) { return (400, Ok(json!({ "error": "hello needs a hex nonce (32+ characters)." }))); }
                let server_nonce = vr_core::crypto::random_hex(16);
                let Ok(key) = HostKey::derive(r["secret"].as_str().unwrap_or(""), &nonce, &server_nonce) else { return (400, Ok(json!({ "error": "bad nonce" }))) };
                let caps: Vec<String> = b["caps"].as_array().map(|a| a.iter().filter_map(|c| c.as_str()).filter(|c| !c.is_empty() && c.len() <= 20 && c.bytes().all(|x| x.is_ascii_lowercase() || x == b'-')).take(20).map(String::from).collect()).unwrap_or_default();
                let engines: Vec<String> = match &b["engines"] { Value::Array(a) => a.iter().filter_map(|e| e.as_str().map(String::from)).collect(), Value::Object(m) => m.keys().cloned().collect(), _ => vec![] };
                let engines: Vec<String> = engines.into_iter().filter(|e| ENGINES.contains(&e.as_str())).collect();
                let user = { let u = r["user"].as_str().unwrap_or(""); if u.is_empty() { b["user"].as_str().unwrap_or("").to_string() } else { u.to_string() } };
                let pc = b["pc"] == json!(true) && (!r["user"].as_str().unwrap_or("").is_empty() || (!user.is_empty() && user.len() <= 32 && user.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')));
                let hello = json!({ "at": now_ms(), "nickname": r["nickname"], "user": user, "providers": Self::clean_providers(&b["providers"]), "engines": engines, "version": b["version"].as_str().unwrap_or("").chars().take(20).collect::<String>(), "os": b["os"].as_str().unwrap_or("").chars().take(20).collect::<String>(), "pc": pc, "caps": caps });
                let kid = key.id();
                { let mut live = self.live.lock().unwrap(); let n = live.entry(id.clone()).or_default(); n.key = Some(key); n.kid = kid.clone(); n.hello = Some(hello.clone()); n.relay_ok = Map::new(); }
                self.save_sessions();
                println!("[ai-host] {} ({}) connected: {} provider(s), engines {}, key {kid}{}", r["nickname"].as_str().unwrap_or(""), if hello["user"].as_str().unwrap_or("").is_empty() { "no owner" } else { hello["user"].as_str().unwrap_or("") }, hello["providers"].as_array().map(|a| a.len()).unwrap_or(0), if hello["engines"].as_array().map(|a| a.is_empty()).unwrap_or(true) { "none".to_string() } else { hello["engines"].as_array().unwrap().iter().filter_map(|e| e.as_str()).collect::<Vec<_>>().join(", ") }, if caps.is_empty() { ", an older host".to_string() } else { format!(", can {}", caps.join(" ")) });
                if !caps.iter().any(|c| c == "relay") { self.probe_relays(id.clone()); }
                (200, Ok(json!({ "id": id, "nickname": r["nickname"], "user": r["user"], "serverNonce": server_nonce, "kid": kid, "pollMs": POLL_MS, "offlineMs": self.offline_ms, "group": clean_users(self.cfg.get("hostGroup").unwrap_or(&Value::Null)), "update": { "version": self.release().0, "commit": self.release().1 } })))
            }
            "/node/update" if get => match self.host_files_manifest() {
                Some(m) => { let sig = vr_core::crypto::hmac_hex(r["secret"].as_str().unwrap_or("").as_bytes(), m.to_string().as_bytes()); let mut out = m.clone(); out["sig"] = json!(sig); (200, Ok(out)) }
                None => (404, Ok(json!({ "error": "No updates here." }))),
            },
            p if get && p.starts_with("/node/update/file/") => {
                let name = &p["/node/update/file/".len()..];
                if !HOST_FILES.contains(&name) { return (404, Ok(json!({ "error": "Not a host file." }))); }
                match std::fs::read(self.root.join(name)) { Ok(buf) => (200, Err((buf, "application/octet-stream"))), Err(_) => (404, Ok(json!({ "error": "Not here." }))) }
            }
            "/node/update/bin" | "/node/update/bin/file" if get => {
                let q: HashMap<String, String> = crate::front::url_params(query).into_iter().collect();
                let app = q.get("app").cloned().unwrap_or_else(|| "vr-host".into()); let target = q.get("target").cloned().unwrap_or_default();
                if app != "vr-host" { return (404, Ok(json!({ "error": "No native releases here." }))); }
                if path == "/node/update/bin" { match native.signed(&app, &target, Some(r["secret"].as_str().unwrap_or("")), &self.release()) { Some(s) => (200, Ok(s)), None => (404, Ok(json!({ "error": format!("Nothing built for {target}.") }))) } }
                else { match native.file(&app, &target, q.get("name").map(String::as_str).unwrap_or("")) { Some(buf) => (200, Err((buf, "application/octet-stream"))), None => (404, Ok(json!({ "error": "Not a file of that release." }))) } }
            }
            "/node/poll" if get => {
                let has_hello = self.live.lock().unwrap().get(&id).map(|n| n.hello.is_some()).unwrap_or(false);
                if !has_hello && hs.contains_key("x-vr-host") { return (if hs.contains_key("x-vr-host-version") { 428 } else { 401 }, Ok(json!({ "error": "Say hello again: this hub does not know what you offer yet.", "rehello": true }))); }
                // listening before looking, so a job queued in between is not missed
                let woke = notify.notified(); tokio::pin!(woke); woke.as_mut().enable();
                let ready = { let live = self.live.lock().unwrap(); live.get(&id).map(|n| !n.queue.is_empty() || !n.cancel.is_empty()).unwrap_or(false) };
                if !ready {
                    // counted as polling while it waits, also when the host hangs up halfway (the guard counts it out)
                    struct Waiter<'a>(&'a AiHosts, String);
                    impl Drop for Waiter<'_> { fn drop(&mut self) { if let Some(n) = self.0.live.lock().unwrap().get_mut(&self.1) { n.waiters = n.waiters.saturating_sub(1); n.seen = now_ms(); } self.0.changed(); } }
                    { self.live.lock().unwrap().entry(id.clone()).or_default().waiters += 1; }
                    let _w = Waiter(self, id.clone());
                    let _ = tokio::time::timeout(Duration::from_millis(POLL_MS), woke).await;
                }
                let (jobs, cancel) = { let mut live = self.live.lock().unwrap(); let n = live.entry(id.clone()).or_default(); let k = n.queue.len().min(8); let jobs: Vec<Value> = n.queue.drain(0..k).collect(); let cancel: Vec<String> = n.cancel.drain().collect(); (jobs, cancel) };
                { let mut p = self.pending.lock().unwrap(); for j in &jobs { if let Some(pj) = p.get_mut(j["id"].as_str().unwrap_or("")) { pj.picked = true; } } }
                (200, Ok(json!({ "jobs": jobs, "cancel": cancel, "beat": self.beat() })))
            }
            "/node/reply" if post => {
                let jid = b["id"].as_str().unwrap_or("").to_string();
                let mut pend = self.pending.lock().unwrap();
                let ok = pend.get(&jid).map(|j| j.host == id && j.kind != "work" && matches!(j.waiting, Some(Waiting::Whole(_)))).unwrap_or(false);
                if !ok { return (404, Ok(json!({ "error": "No such job." }))); }
                let j = pend.remove(&jid).unwrap(); drop(pend);
                if let Some(n) = self.live.lock().unwrap().get_mut(&id) { n.queue.retain(|x| x["id"] != json!(jid)); }
                let body = vr_core::unb64(b["body"].as_str().unwrap_or("")).unwrap_or_default();
                let (tx, rx) = mpsc::channel(2); if !body.is_empty() { let _ = tx.try_send(Ok(Bytes::from(body))); } drop(tx);
                if let Some(Waiting::Whole(w)) = j.waiting { let _ = w.send(Ok(Relayed { status: host_status(&b["status"]), headers: pick_headers(&b["headers"]), body: rx })); }
                (200, Ok(json!({ "ok": true })))
            }
            "/node/stream" if post => {
                let jid = b["id"].as_str().unwrap_or("").to_string();
                let (sender, first) = {
                    let mut pend = self.pending.lock().unwrap();
                    let Some(j) = pend.get_mut(&jid).filter(|j| j.host == id && j.kind != "work") else { return (404, Ok(json!({ "error": "No such job." }))) };
                    match j.waiting.take() {
                        Some(Waiting::Whole(w)) => { let (tx, rx) = mpsc::channel(256); let _ = w.send(Ok(Relayed { status: host_status(&b["status"]), headers: pick_headers(&b["headers"]), body: rx })); j.waiting = Some(Waiting::Stream(tx.clone())); (Some(tx), true) }
                        Some(Waiting::Stream(tx)) => { j.waiting = Some(Waiting::Stream(tx.clone())); (Some(tx), false) }
                        other => { j.waiting = other; (None, false) }
                    }
                };
                let _ = first;
                if let (Some(tx), Some(c)) = (&sender, b["chunk"].as_str()) { if tx.send(Ok(Bytes::from(vr_core::unb64(c).unwrap_or_default()))).await.is_err() { self.finish(&jid, Some("cancelled".into()), None); return (404, Ok(json!({ "error": "No longer wanted." }))); } }
                if b["done"] == json!(true) { let e = b["error"].as_str().map(String::from); if let Some(p) = self.pending.lock().unwrap().remove(&jid) { if let (Some(Waiting::Stream(tx)), Some(e)) = (p.waiting, e) { let _ = tx.try_send(Err(e)); } } }
                (200, Ok(json!({ "ok": true })))
            }
            "/node/events" if post => {
                let jid = b["job"].as_str().unwrap_or("").to_string();
                let (valid, prov) = { let p = self.pending.lock().unwrap(); p.get(&jid).map(|j| (j.host == id && j.kind == "work", j.provider.clone())).unwrap_or((false, String::new())) };
                if !valid { return (404, Ok(json!({ "error": "No such job." }))); }
                let key = self.live.lock().unwrap().get(&id).and_then(|n| n.key.clone());
                let env: Option<Envelope> = serde_json::from_value(b["enc"].clone()).ok();
                let Some(batch) = key.zip(env).and_then(|(k, e)| k.open(&e).ok()) else { return (400, Ok(json!({ "error": "Could not open the envelope (the keys differ: say hello again)." }))) };
                if let Some(p) = self.pending.lock().unwrap().get_mut(&jid) { p.last = now_ms(); }
                if batch["usage"].is_object() { self.note_host_usage(&id, &prov, &batch["usage"]); }
                let tx = { let p = self.pending.lock().unwrap(); p.get(&jid).and_then(|j| if let Some(Waiting::Work(t)) = &j.waiting { Some(t.clone()) } else { None }) };
                if let Some(tx) = &tx { for ev in batch["events"].as_array().cloned().unwrap_or_default() { let _ = tx.send(WorkMsg::Event(ev)); } }
                let cancelled = self.live.lock().unwrap().get(&id).map(|n| n.cancel.contains(&jid)).unwrap_or(false);
                if batch["done"] == json!(true) { self.finish(&jid, None, Some(batch["code"].as_i64().unwrap_or(0) as i32)); }
                (200, Ok(json!({ "ok": true, "cancel": cancelled })))
            }
            _ => (404, Ok(json!({ "error": "Not found" }))),
        }
    }
    /// The JavaScript host's files as this hub runs them (name + sha256 + size), for its self-update.
    fn host_files_manifest(&self) -> Option<Value> {
        let files: Vec<Value> = HOST_FILES.iter().filter_map(|f| std::fs::read(self.root.join(f)).ok().map(|b| json!({ "name": f, "sha256": vr_core::crypto::sha256_hex(&b), "size": b.len() }))).collect();
        let (v, c) = self.release();
        if files.is_empty() { None } else { Some(json!({ "version": v, "commit": c, "files": files })) }
    }

    // ---------- Settings → My AI hosts ----------
    fn pub_host(&self, r: &Value) -> Value {
        let id = r["id"].as_str().unwrap_or("");
        let live = self.live.lock().unwrap(); let n = live.get(id);
        let providers: Vec<Value> = n.and_then(|n| n.hello.as_ref().map(|h| h["providers"].as_array().cloned().unwrap_or_default().into_iter().map(|p| { let mut x = p.clone(); x["allowedUsers"] = Self::allowed(r, &p); x["declared"] = p["allowedUsers"].clone(); x["usage"] = self.usage_of(id, n, &p); let (c, until) = self.cap_of(id, n, &p); x["capped"] = json!(c); x["cappedUntil"] = json!(until); x }).collect())).unwrap_or_default();
        let online = n.map(|n| n.waiters > 0 || now_ms().saturating_sub(n.seen) < self.offline_ms).unwrap_or(false);
        json!({ "id": id, "nickname": r["nickname"], "user": r["user"], "legacy": r["legacy"].as_bool().unwrap_or(false), "createdAt": r["createdAt"], "online": online,
            "seen": n.filter(|n| n.seen > 0).map(|n| json!(super::auth::iso_at(n.seen as i64))).unwrap_or(Value::Null), "kid": n.map(|n| n.kid.clone()).unwrap_or_default(),
            "version": n.and_then(|n| n.hello.as_ref().map(|h| h["version"].clone())).unwrap_or(json!("")), "os": n.and_then(|n| n.hello.as_ref().map(|h| h["os"].clone())).unwrap_or(json!("")),
            "engines": n.and_then(|n| n.hello.as_ref().map(|h| h["engines"].clone())).unwrap_or(json!([])), "providers": providers })
    }
    pub fn mine(&self, user: &str) -> Vec<Value> { let recs: Vec<Value> = self.db.lock().unwrap()["hosts"].as_object().map(|m| m.values().filter(|r| r["user"] == json!(user)).cloned().collect()).unwrap_or_default(); recs.iter().map(|r| self.pub_host(r)).collect() }
    pub fn all(&self) -> Vec<Value> { self.all_records().iter().map(|r| self.pub_host(r)).collect() }
    fn new_nickname(&self) -> String {
        let pick = |n: usize| { let b = vr_core::crypto::random_bytes(4); (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) % n };
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
        if self.db.lock().unwrap()["hosts"].as_object().map(|m| m.values().filter(|r| r["user"] == json!(user)).count()).unwrap_or(0) >= 10 { return Err("Ten hosts per person is plenty; remove one first.".into()); }
        let nick = if nickname.is_empty() { self.new_nickname() } else { self.check_nick(nickname)? };
        let id = uuid4();
        let r = json!({ "id": id, "user": user, "nickname": nick, "secret": vr_core::crypto::random_hex(24), "createdAt": super::auth::iso_now(), "share": {} });
        self.db.lock().unwrap()["hosts"][&id] = r.clone(); self.save();
        Ok(self.pub_host(&r))
    }
    pub fn rename(&self, user: &str, id: &str, nick: &str) -> Result<Value, String> {
        let owned = self.db.lock().unwrap()["hosts"].get(id).map(|r| r["user"] == json!(user)).unwrap_or(false);
        if !owned { return Err("Not your host.".into()); }
        let n = self.check_nick(nick)?;
        self.db.lock().unwrap()["hosts"][id]["nickname"] = json!(n); self.save();
        Ok(self.pub_host(&self.record(id).unwrap()))
    }
    pub fn remove(&self, user: &str, id: &str) -> Result<(), String> {
        let owned = self.db.lock().unwrap()["hosts"].get(id).map(|r| r["user"] == json!(user)).unwrap_or(false);
        if !owned { return Err("Not your host.".into()); }
        if let Some(m) = self.db.lock().unwrap()["hosts"].as_object_mut() { m.remove(id); }
        self.live.lock().unwrap().remove(id); self.save();
        Ok(())
    }
    pub fn set_share(&self, user: &str, id: &str, pid: &str, users: &Value) -> Result<Value, String> {
        let owned = self.db.lock().unwrap()["hosts"].get(id).map(|r| r["user"] == json!(user)).unwrap_or(false);
        if !owned { return Err("Not your host.".into()); }
        if pid.is_empty() || pid.len() > 32 || !pid.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') { return Err("Unknown provider.".into()); }
        { let mut db = self.db.lock().unwrap(); let r = &mut db["hosts"][id]; if !r["share"].is_object() { r["share"] = json!({}); }
          if users.is_null() { if let Some(m) = r["share"].as_object_mut() { m.remove(pid); } } else { r["share"][pid] = json!(clean_users(users)); } }
        self.save();
        Ok(self.pub_host(&self.record(id).unwrap()))
    }
    /// The host program's config for one of the person's hosts (what the AI Host download carries).
    pub fn config_for(&self, user: &str, id: &str, hub: &str, models: Vec<String>) -> Result<Value, String> {
        let r = self.db.lock().unwrap()["hosts"].get(id).cloned().filter(|r| r["user"] == json!(user)).ok_or("Not your host.")?;
        let nick = r["nickname"].as_str().unwrap_or("");
        Ok(json!({ "_": format!("VibeRoller AI Host \"{nick}\" of {user}: made in Settings → My AI hosts. Keep this file private: the token is this host's key to the hub."), "hub": hub, "id": r["id"], "nickname": nick, "user": user, "token": r["secret"],
            "providers": [{ "id": "claude", "kind": "claude-relay", "label": format!("{user}'s Claude"), "models": models, "max5h": 90, "max7d": 90, "shareWithGroup": true, "allowedUsers": [] },
                { "id": "codex", "kind": "codex-relay", "label": format!("{user}'s ChatGPT / Codex"), "models": ["gpt-5.5"], "shareWithGroup": true, "allowedUsers": [] }],
            "engines": { "ollama": "http://127.0.0.1:11434", "image": "http://127.0.0.1:5124", "voice": "http://127.0.0.1:5123", "speech": "http://127.0.0.1:5125" },
            "autoUpdate": true, "update": { "quietMinutes": 3, "checkMinutes": 5 }, "reconnect": { "maxDelaySeconds": 5 }, "security": { "engineGuard": true, "allowInsecureHub": false } }))
    }
    pub fn enabled(&self) -> bool { !self.all_records().is_empty() }
}
pub fn release_now(root: &std::path::Path) -> (String, String) {
    let mills = root.join("..");
    let version = std::fs::read_to_string(mills.join("versions.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["project"].as_str().map(String::from)).unwrap_or_else(|| "0.0.0".into());
    (version, super::worktree::git(&mills, &["rev-parse", "HEAD"]).unwrap_or_default())
}
/// A host's status code, when it is one an HTTP answer can carry (else 502): a host never decides what breaks here.
fn host_status(v: &Value) -> u16 { v.as_u64().filter(|s| (100..=599).contains(s)).map(|s| s as u16).unwrap_or(502) }
/// The headers of a host's answer that are passed on: known names only, and only ones that are valid HTTP headers.
fn pick_headers(v: &Value) -> Vec<(String, String)> {
    let ok = |k: &str| { let k = k.to_lowercase(); matches!(k.as_str(), "content-type" | "request-id" | "retry-after") || k.starts_with("anthropic-ratelimit-") || k.starts_with("x-codex-") || k.starts_with("openai-") };
    v.as_object().map(|m| m.iter().filter(|(k, v)| ok(k) && v.as_str().map(|s| hyper::header::HeaderValue::from_str(s).is_ok()).unwrap_or(false) && hyper::header::HeaderName::from_bytes(k.to_lowercase().as_bytes()).is_ok()).map(|(k, v)| (k.to_lowercase(), v.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default()
}
fn uuid4() -> String { let b = vr_core::crypto::random_bytes(16); let mut b: [u8; 16] = b.try_into().expect("16"); b[6] = (b[6] & 0x0f) | 0x40; b[8] = (b[8] & 0x3f) | 0x80; let h = hex::encode(b); format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]) }
