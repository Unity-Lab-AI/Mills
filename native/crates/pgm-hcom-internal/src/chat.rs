//! The chat: end-to-end encrypted messages (the server keeps public keys, password-wrapped key backups it cannot open,
//! and ciphertext; each reader gets only their own key slot), receipts, typing, presence, the room's long poll, calls
//! (set-up relay + who is in which call), pins / servers / channels / groups ([`crate::guilds`]), the team tools of AI
//! runs, and the chat's AI member: her key (Chat/agent-key.json, the only key the server holds), what she is sent, and
//! what she posts. Her brain is not here: [`ChatHost::agent_called`] says she was called. Files: Chat/messages.jsonl,
//! keys.json, seen.json, guilds.json, agent-key.json; agent-seen.json (what she has read).
//! Everything this needs from the program around it comes through [`ChatHost`] (VibeRoller's hub implements it).
use pgm_core_internal::config::Config;
use pgm_identity_internal::Account;
use pgm_net_internal::http::{json as resp_json, Body, Req};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use crate::e2e::{ChatKey, Incoming, PubJwk, Wrapped};
use crate::files::Files;
use crate::guilds::{self, Ctx, Guilds};
use pgm_core_internal::now_ms;

/// What the chat needs from the program around it.
pub trait ChatHost: Send + Sync {
    fn chat(&self) -> &Arc<Chat>;
    fn files(&self) -> &Files;
    fn accounts(&self) -> Vec<Account>;
    fn account(&self, u: &str) -> Option<Account>;
    /// How a person is named (their nickname, else their display name).
    fn name_of(&self, u: &str) -> String;
    /// The program's folder (its `ai-host.off` switch and `.rolled-back` note are read there) and its settings.
    fn root(&self) -> &Path;
    fn cfg(&self) -> &Config;
    /// The shared AI sessions this person may watch (for the room's answer).
    fn shares_for(&self, u: &str) -> Vec<Value>;
    /// This person's AI usage windows (the team tools' hub_status).
    fn usage_status(&self, a: &Account) -> Value;
    /// The team notes' version (the room's answer names it, so pages notice a change).
    fn notes_version(&self) -> u64;
    /// The AI hosts and whether each is up (the team tools' hub_status).
    fn ai_hosts_status(&self) -> Vec<Value>;
    /// The chat's AI member was called: answer `target` privately ('' = where she was called), with `caller`'s tools,
    /// in `conv` (a channel / group, '' = # Everyone).
    fn agent_called(&self, target: String, caller: String, conv: String);
    /// Is any run or session of this person's busy (presence's "working"; plans/presence.md)? Its changes must reach
    /// [`Chat::room_changed`].
    fn working(&self, _u: &str) -> bool { false }
    /// How many of this person's sessions work now (shown next to "working").
    fn working_count(&self, u: &str) -> u64 { u64::from(self.working(u)) }
}
/// An answer.
pub type Resp = hyper::Response<Body>;
fn answer(status: u16, v: Value) -> Resp { resp_json(status, &v) }

pub const AGENT: &str = "claude";
const MAX_CT: usize = 1_600_000;
const TYPING_MS: u64 = 6000; const GONE_MS: u64 = 8000; const ONLINE_MS: u64 = 70000;
const ICE_DEFAULT: &str = r#"[{"urls":"stun:stun.l.google.com:19302"}]"#;

/// One person's presence (plans/presence.md): `online` / `idle` are worked out from their devices (the most active one
/// wins), `screen` is the last one any device named.
#[derive(Default, Clone)]
struct Pres { online: bool, idle: bool, screen: String, at: u64, seen: u64, devices: HashMap<String, Dev> }
/// One page / app instance (its `device` id; an older page without one is "legacy"): live while it holds a long poll
/// (and 8 s after the last one closed), idle when it says so (5 min without input, hidden, or the screen locked).
#[derive(Default, Clone)]
struct Dev { polls: u32, gen: u64, live: bool, idle: bool, locked: bool, seen: u64 }
impl Pres {
    /// online = any live device; idle (away) = every live device idle
    fn derive(&mut self) {
        let live: Vec<&Dev> = self.devices.values().filter(|d| d.live).collect();
        self.online = !live.is_empty();
        self.idle = self.online && live.iter().all(|d| d.idle || d.locked);
    }
    fn dev(&mut self, id: &str) -> &mut Dev { self.devices.entry(id.to_string()).or_default() }
}
/// A device id as a page sends it: letters, digits, - and _, up to 40 ("" = an older page: "legacy").
fn device_id(v: Option<&str>) -> String { match v.map(str::trim).filter(|x| !x.is_empty() && x.len() <= 40 && x.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')) { Some(x) => x.to_string(), None => "legacy".into() } }
/// What a person set about their presence (state.json "presence", and profile.status / profile.textUntil): read once,
/// kept until they change it (or their profile changes: keys_changed).
#[derive(Default, Clone)]
struct Manual { over: String, over_until: u64, dnd: bool, dnd_until: u64, accepting: bool, hide_where: bool, text: String, text_until: u64 }
impl Manual {
    fn from_state(st: &Value) -> Manual {
        let p = &st["presence"]; let pr = &st["profile"];
        Manual { over: s(p, "override"), over_until: p["overrideUntil"].as_u64().unwrap_or(0), dnd: p["dnd"] == json!(true), dnd_until: p["dndUntil"].as_u64().unwrap_or(0), accepting: p["acceptingWork"] == json!(true), hide_where: p["hideWhere"] == json!(true), text: s(pr, "status"), text_until: pr["textUntil"].as_u64().unwrap_or(0) }
    }
    /// what holds right now (an `until` that passed is as if it was never set)
    fn now(&self, now: u64) -> Manual {
        let mut m = self.clone();
        if m.over_until > 0 && now >= m.over_until { m.over = String::new(); m.over_until = 0; }
        if !matches!(m.over.as_str(), "away" | "offline") { m.over = "auto".into(); }
        if m.dnd_until > 0 && now >= m.dnd_until { m.dnd = false; m.dnd_until = 0; }
        if m.text_until > 0 && now >= m.text_until { m.text = String::new(); m.text_until = 0; }
        m
    }
}
/// The label others see (plans/presence.md § 2): the first rule that matches.
fn label_of(shown: &str, over: &str, working: bool, accepting: bool) -> &'static str {
    if over == "offline" { return "Offline"; }
    if working && accepting && shown != "offline" { return "Working + available"; }
    if working { return "Working"; }
    match shown { "offline" => "Offline", "away" => "Away", _ if accepting => "Available for work", _ => "Online" }
}
struct Store { messages: Vec<Value>, seq: i64, rev: i64 }
#[derive(Default)]
struct Calls { calls: Vec<(String, Vec<(String, Value)>)>, mailbox: HashMap<String, Vec<Value>>, n: u64, ver: u64 }

pub struct Chat {
    dir: PathBuf,
    store: Mutex<Store>,
    keys: Mutex<Map<String, Value>>,
    seen: Mutex<Map<String, Value>>,
    seen_dirty: Notify,
    pub guilds: Guilds,
    room_ver: AtomicU64,
    keys_ver: AtomicU64,
    room_woke: Notify,
    presence: Mutex<HashMap<String, Pres>>,
    /// what each person set (read from their state.json once; dropped when their profile changes)
    manual: Mutex<HashMap<String, Manual>>,
    /// each person's last label and since when (the label's `since`)
    labels: Mutex<HashMap<String, (String, u64)>>,
    typing: Mutex<HashMap<String, (String, u64)>>,
    agent_key: ChatKey,
    agent_pub: Value,
    /// Unity's queue: every message up to .0 is answered, and these above it too (answered in their own conversation's turn)
    agent_seen: Mutex<(i64, std::collections::BTreeSet<i64>)>,
    /// Unity composing: (target, conv) while her brain is answering
    agent_busy: Mutex<Option<(String, String)>>,
    calls: Mutex<Calls>,
    call_woke: Mutex<HashMap<String, Arc<Notify>>>,
    avatars: Mutex<HashMap<String, String>>,
    agent_name: String,
    agent_on: bool,
    handle: String,
    mills: PathBuf,
}
use pgm_core_internal::json::{read_json, s, write_json};
fn b64ok(v: &Value, max: usize) -> bool { v.as_str().map(|x| x.len() <= max && x.bytes().all(|c| c.is_ascii_alphanumeric() || b"+/=_-".contains(&c))).unwrap_or(false) }
fn dm_of(conv: &str) -> Option<(String, String)> { match guilds::parse(conv) { Some(guilds::Conv::Dm(a, b)) => Some((a, b)), _ => None } }
fn iso() -> String { pgm_core_internal::time::iso_now() }

/// What one reader may see, worked out once (servers and groups first, then the messages: never both locked at once).
pub struct Reader { user: String, convs: HashMap<String, (bool, bool, i64)> } // conv → (member, reads history, joined at seq)
impl Reader {
    fn sees_conv(&self, conv: &str) -> bool { conv == "room" || dm_of(conv).map(|(a, b)| a == self.user || b == self.user).unwrap_or(false) || self.convs.get(conv).map(|c| c.0).unwrap_or(false) }
    fn reads(&self, conv: &str, seq: i64) -> bool { match self.convs.get(conv) { Some((_, h, j)) => *h || seq > *j, None => true } }
    pub fn visible(&self, m: &Value) -> bool {
        if m["from"] == json!(self.user) { return true; }
        let seq = m["seq"].as_i64().unwrap_or(0);
        if let Some(c) = m["conv"].as_str().filter(|c| !c.is_empty()) { return self.convs.get(c).map(|x| x.0).unwrap_or(false) && self.reads(c, seq); }
        if let Some(t) = m["to"].as_str().filter(|t| !t.is_empty()) { return t == self.user; }
        self.reads("room", seq)
    }
}
fn conv_of(m: &Value) -> String { if let Some(c) = m["conv"].as_str().filter(|c| !c.is_empty()) { return c.into(); } if let Some(t) = m["to"].as_str().filter(|t| !t.is_empty()) { let (a, b) = { let f = s(m, "from"); if f.as_str() < t { (f, t.to_string()) } else { (t.to_string(), f) } }; return format!("dm:{a}|{b}"); } "room".into() }

impl Chat {
    /// `mills`: the folder Chat/ (and Avatars/) live in; `root`: the folder server-owners.json is in.
    pub fn new(mills: &Path, root: &Path, cfg: &Config, users: &[(String, String, bool)]) -> Chat {
        let dir = mills.join("Chat"); let _ = std::fs::create_dir_all(&dir);
        let messages: Vec<Value> = std::fs::read_to_string(dir.join("messages.jsonl")).unwrap_or_default().lines().filter(|l| !l.is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect();
        let seq = messages.iter().map(|m| m["seq"].as_i64().unwrap_or(0)).max().unwrap_or(0);
        let rev = messages.iter().map(|m| m["rev"].as_i64().or(m["seq"].as_i64()).unwrap_or(0)).max().unwrap_or(0);
        let keys = read_json(&dir.join("keys.json")).and_then(|v| v.as_object().cloned()).unwrap_or_default();
        let seen = read_json(&dir.join("seen.json")).and_then(|v| v.as_object().cloned()).unwrap_or_default();
        // Unity's key: made once (the JavaScript hub made it before), kept here
        let kf = dir.join("agent-key.json");
        let (agent_key, agent_pub) = match read_json(&kf).filter(|k| k["priv"]["d"].is_string()).and_then(|k| ChatKey::from_jwk(&k["priv"].to_string()).ok().map(|key| (key, k["pub"].clone()))) {
            Some(x) => x,
            None => { let key = ChatKey::generate(); let p = key.public_jwk(); let pubv = json!({ "kty": "EC", "crv": "P-256", "x": p.x, "y": p.y }); let privv: Value = serde_json::from_str(&key.to_jwk()).unwrap_or(json!({})); write_json(&kf, &json!({ "priv": privv, "pub": pubv })); (key, pubv) }
        };
        let sv = read_json(&dir.join("agent-seen.json"));
        let agent_seen = (sv.as_ref().and_then(|v| v["lastSeq"].as_i64()).or_else(|| read_json(&dir.join("agent-state.json")).and_then(|v| v["lastSeq"].as_i64())).unwrap_or(0),
            sv.and_then(|v| v["handled"].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect())).unwrap_or_default());
        let agent_name = cfg.get("roomAgentName").and_then(Value::as_str).unwrap_or("Unity").to_string();
        let handle: String = agent_name.to_lowercase().split_whitespace().collect();
        let agent_on = !users.iter().any(|u| u.0 == AGENT) && cfg.get("roomAgent") != Some(&json!(false));
        let home = cfg.get("homeServerName").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).unwrap_or("VibeRoller");
        let g = Guilds::new(&dir, root.join("server-owners.json"), &agent_name, home, users);
        Chat { dir, store: Mutex::new(Store { messages, seq, rev }), keys: Mutex::new(keys), seen: Mutex::new(seen), seen_dirty: Notify::new(), guilds: g, room_ver: AtomicU64::new(1), keys_ver: AtomicU64::new(1), room_woke: Notify::new(),
            presence: Mutex::default(), manual: Mutex::default(), labels: Mutex::default(), typing: Mutex::default(), agent_key, agent_pub, agent_seen: Mutex::new(agent_seen), agent_busy: Mutex::new(None), calls: Mutex::default(), call_woke: Mutex::default(), avatars: Mutex::default(), agent_name, agent_on, handle, mills: mills.to_path_buf() }
    }
    pub fn room_changed(&self) { self.room_ver.fetch_add(1, Ordering::SeqCst); self.room_woke.notify_waiters(); }
    pub fn keys_changed(&self) { self.keys_ver.fetch_add(1, Ordering::SeqCst); self.avatars.lock().unwrap_or_else(|e| e.into_inner()).clear(); self.manual.lock().unwrap_or_else(|e| e.into_inner()).clear(); self.room_changed(); }
    pub fn has_key(&self, u: &str) -> bool { self.keys.lock().unwrap_or_else(|e| e.into_inner()).contains_key(u) }
    fn pub_of(&self, u: &str) -> Option<Value> { self.keys.lock().unwrap_or_else(|e| e.into_inner()).get(u).map(|k| k["pub"].clone()) }
    fn save_keys(&self) { write_json(&self.dir.join("keys.json"), &Value::Object(self.keys.lock().unwrap_or_else(|e| e.into_inner()).clone())); }
    pub fn drop_key(&self, u: &str) { if self.keys.lock().unwrap_or_else(|e| e.into_inner()).remove(u).is_some() { self.save_keys(); } }
    fn agent_enabled(&self, users: &[(String, String, bool)]) -> bool { self.agent_on && !users.iter().any(|u| u.0 == AGENT) }
    pub fn agent_on(&self, users: &[(String, String, bool)]) -> bool { self.agent_enabled(users) }
    /// Unity composing (target, conv) or not: everyone's "is typing".
    pub fn set_agent_busy(&self, v: Option<(String, String)>) { *self.agent_busy.lock().unwrap_or_else(|e| e.into_inner()) = v; self.room_changed(); }
    pub fn seq(&self) -> i64 { self.store.lock().unwrap_or_else(|e| e.into_inner()).seq }
    /// The conversation of a message (None when deleted or unknown): pins and the guilds ask.
    pub fn msg_conv(&self, seq: i64) -> Option<String> { self.store.lock().unwrap_or_else(|e| e.into_inner()).messages.iter().find(|m| m["seq"].as_i64() == Some(seq) && m["deleted"] != json!(true)).map(conv_of) }
    fn save_messages(store: &Store, dir: &Path) {
        let f = dir.join("messages.jsonl"); let tmp = f.with_extension("jsonl.tmp");
        let mut out = String::new(); for m in &store.messages { out.push_str(&m.to_string()); out.push('\n'); }
        if std::fs::write(&tmp, out).is_ok() { let _ = std::fs::rename(&tmp, &f); }
    }
    fn append(&self, mut msg: Value) -> i64 {
        let seqn = msg["seq"].as_i64().unwrap_or(0);
        { let mut st = self.store.lock().unwrap_or_else(|e| e.into_inner()); st.rev += 1; msg["rev"] = json!(st.rev);
          use std::io::Write; if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("messages.jsonl")) { let _ = writeln!(f, "{msg}"); }
          st.messages.push(msg); }
        self.room_changed(); seqn
    }
    /// Deletes a person's messages in these conversations since then (a ban that deletes): their readers lose the files.
    fn delete_recent(&self, user: &str, convs: &[String], since: i64, files: &Files, acct: &dyn Fn(&str) -> Option<Account>) -> usize {
        let mut gone_files: Vec<String> = vec![]; let mut n = 0;
        { let mut st = self.store.lock().unwrap_or_else(|e| e.into_inner()); let mut rev = st.rev;
          for m in st.messages.iter_mut() {
              let ts = chrono::DateTime::parse_from_rfc3339(m["ts"].as_str().unwrap_or("")).map(|d| d.timestamp_millis()).unwrap_or(0);
              if m["from"] == json!(user) && m["deleted"] != json!(true) && m["conv"].as_str().map(|c| convs.iter().any(|x| x == c)).unwrap_or(false) && ts >= since {
                  tombstone(m, &mut gone_files); rev += 1; m["rev"] = json!(rev); n += 1;
              }
          }
          st.rev = rev; if n > 0 { Self::save_messages(&st, &self.dir); } }
        if !gone_files.is_empty() { files.message_gone(&gone_files, acct); }
        if n > 0 { self.room_changed(); }
        n
    }
}
fn tombstone(m: &mut Value, gone_files: &mut Vec<String>) {
    let o = m.as_object_mut().unwrap();
    for k in ["iv", "ct", "keys", "fromPub", "editedAt", "blob"] { o.remove(k); }
    if let Some(f) = o.remove("files") { gone_files.extend(f.as_array().cloned().unwrap_or_default().into_iter().filter_map(|x| x.as_str().map(String::from))); }
    o.insert("deleted".into(), json!(true)); o.insert("keys".into(), json!({}));
}

// ---------- the hub around the chat ----------
fn users(hub: &dyn ChatHost) -> Vec<(String, String, bool)> { hub.accounts().into_iter().map(|a| (a.username, a.display, a.admin)).collect() }
/// One call's Ctx for the guilds (the accounts now, the chat's seq, pins' conversations, a ban's deletes).
macro_rules! ctx { ($hub:expr, $users:expr) => { Ctx { users: &$users, seq: $hub.chat().seq(), msg_conv: &|n| $hub.chat().msg_conv(n), delete_recent: &|u: &str, c: &[String], since: i64| $hub.chat().delete_recent(u, c, since, &$hub.files(), &|x| $hub.account(x)) } } }
fn reader(hub: &dyn ChatHost, user: &str, us: &[(String, String, bool)]) -> Reader {
    let c = &hub.chat(); let ctx = ctx!(hub, us.to_vec());
    let mut convs = HashMap::new();
    let all: Vec<String> = c.guilds.with(|db| {
        let mut v = vec!["room".to_string()];
        for sv in db["servers"].as_array().cloned().unwrap_or_default() { for ch in sv["channels"].as_array().cloned().unwrap_or_default() { if !(sv["everyone"] == json!(true) && ch["id"] == json!("general")) { v.push(format!("ch:{}/{}", s(&sv, "id"), s(&ch, "id"))); } } }
        for g in db["groups"].as_array().cloned().unwrap_or_default() { v.push(format!("grp:{}", s(&g, "id"))); }
        v
    });
    for conv in all {
        let member = conv == "room" || c.guilds.is_member(&conv, user, &ctx);
        let hist = c.guilds.can_read(&conv, user, i64::MAX, &ctx) && c.guilds.can_read(&conv, user, 0, &ctx);
        let joined = c.guilds.with(|db| Guilds::chan_of(db, &conv).map(|(sv, _)| sv["joined"][user]["seq"].as_i64().unwrap_or(if sv["joined"][user].is_object() { 0 } else { i64::MAX })).unwrap_or(0));
        convs.insert(conv, (member, hist, joined));
    }
    Reader { user: user.to_string(), convs }
}
/// May this person read message `seq` (and so its files)?
pub fn may_read(hub: &dyn ChatHost, user: &str, seq: i64) -> bool {
    let us = users(hub); let rd = reader(hub, user, &us);
    let st = hub.chat().store.lock().unwrap_or_else(|e| e.into_inner());
    st.messages.iter().find(|m| m["seq"].as_i64() == Some(seq)).map(|m| m["deleted"] != json!(true) && rd.visible(m) && !m["keys"][user].is_null()).unwrap_or(false)
}
fn conv_member(hub: &dyn ChatHost, conv: &str, user: &str, us: &[(String, String, bool)]) -> bool {
    if conv == "room" { return true; }
    if let Some(r) = conv.strip_prefix("dm:") { return r.split('|').any(|x| x == user); }
    let ctx = ctx!(hub, us.to_vec()); hub.chat().guilds.is_member(conv, user, &ctx)
}

// ---------- presence + typing ----------
impl Chat {
    fn pres_set(&self, user: &str, f: impl FnOnce(&mut Pres)) {
        let changed = { let mut m = self.presence.lock().unwrap_or_else(|e| e.into_inner()); let p = m.entry(user.to_string()).or_default(); let before = (p.online, p.idle, p.screen.clone()); f(p); p.derive(); p.seen = now_ms(); let after = (p.online, p.idle, p.screen.clone()); if before != after { p.at = now_ms(); true } else { false } };
        if changed { self.room_changed(); }
    }
    pub fn touch(&self, user: &str) { self.presence.lock().unwrap_or_else(|e| e.into_inner()).entry(user.to_string()).or_default().seen = now_ms(); }
    fn poll_opened(&self, user: &str, dev: &str) { self.pres_set(user, |p| { let d = p.dev(dev); d.polls += 1; d.gen += 1; d.live = true; d.seen = now_ms(); }); }
    fn poll_closed(self: &Arc<Self>, user: &str, dev: &str) {
        let gen = { let mut m = self.presence.lock().unwrap_or_else(|e| e.into_inner()); let d = m.entry(user.to_string()).or_default().dev(dev); d.polls = d.polls.saturating_sub(1); d.seen = now_ms(); if d.polls > 0 { return; } d.gen += 1; d.gen };
        let (c, u, dv) = (self.clone(), user.to_string(), dev.to_string());
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(GONE_MS)).await; let still = c.presence.lock().unwrap_or_else(|e| e.into_inner()).get(&u).and_then(|p| p.devices.get(&dv)).map(|d| d.gen == gen && d.polls == 0 && d.live).unwrap_or(false); if still { c.pres_set(&u, |p| { let d = p.dev(&dv); d.live = false; d.idle = false; }); } });
    }
    /// Every device of this person gone at once (signed out, or an older page's pagehide).
    pub fn gone(&self, user: &str) { self.pres_set(user, |p| { for d in p.devices.values_mut() { d.polls = 0; d.gen += 1; d.live = false; d.idle = false; } }); }
    /// One device gone at once (its page was closed).
    fn device_gone(&self, user: &str, dev: &str) { self.pres_set(user, |p| { let d = p.dev(dev); d.polls = 0; d.gen += 1; d.live = false; d.idle = false; }); }
    /// Devices not seen for ONLINE_MS are gone (a page that vanished without saying so).
    fn expire(p: &mut Pres, now: u64) { let mut lapsed = false; for d in p.devices.values_mut() { if d.live && now.saturating_sub(d.seen) > ONLINE_MS && d.polls == 0 { d.live = false; d.idle = false; lapsed = true; } } if lapsed { let was = (p.online, p.idle); p.derive(); if was != (p.online, p.idle) { p.at = now; } } }
    /// The old presence list (older pages and the team tools read it): what others are shown, so it applies a person's
    /// "appear offline / away".
    pub fn presence_list(&self, us: &[(String, String, bool)]) -> Vec<Value> {
        let now = now_ms(); let manual = self.manual.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut m = self.presence.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<(String, Value)> = vec![];
        for (u, p) in m.iter_mut() {
            if !us.iter().any(|x| x.0 == *u) { continue; }
            Chat::expire(p, now);
            let over = manual.get(u).map(|x| x.now(now).over).unwrap_or_default();
            let online = p.online && over != "offline"; let idle = online && (p.idle || over == "away");
            out.push((u.clone(), json!({ "user": u, "online": online, "idle": idle, "screen": if online { p.screen.clone() } else { String::new() }, "at": p.at, "seen": p.seen })));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0)); out.into_iter().map(|x| x.1).collect()
    }
    /// Where a person really is (online / away / offline) and how many devices are live.
    fn real_presence(&self, u: &str, now: u64) -> (&'static str, u64, usize) {
        let mut m = self.presence.lock().unwrap_or_else(|e| e.into_inner());
        match m.get_mut(u) { None => ("offline", 0, 0), Some(p) => { Chat::expire(p, now); (if !p.online { "offline" } else if p.idle { "away" } else { "online" }, p.seen, p.devices.values().filter(|d| d.live).count()) } }
    }
    pub fn online(&self, us: &[(String, String, bool)]) -> Vec<String> { self.presence_list(us).into_iter().filter(|p| p["online"] == json!(true)).map(|p| s(&p, "user")).collect() }
    pub fn set_typing(self: &Arc<Self>, user: &str, conv: &str, on: bool) {
        let prev = self.typing.lock().unwrap_or_else(|e| e.into_inner()).remove(user);
        if !on { if prev.is_some() { self.room_changed(); } return; }
        let at = now_ms(); self.typing.lock().unwrap_or_else(|e| e.into_inner()).insert(user.to_string(), (conv.to_string(), at));
        let (c, u) = (self.clone(), user.to_string());
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(TYPING_MS)).await; let lapsed = { let mut t = c.typing.lock().unwrap_or_else(|e| e.into_inner()); if t.get(&u).map(|x| x.1 == at).unwrap_or(false) { t.remove(&u); true } else { false } }; if lapsed { c.room_changed(); } });
        if prev.map(|p| p.0 != conv).unwrap_or(true) { self.room_changed(); }
    }
}
fn typing_for(hub: &dyn ChatHost, user: &str, us: &[(String, String, bool)]) -> Vec<Value> {
    let c = &hub.chat();
    let list: Vec<(String, String)> = c.typing.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(u, t)| (u.clone(), t.0.clone())).collect();
    let mut out: Vec<Value> = list.into_iter().filter(|(u, conv)| u != user && conv_member(hub, conv, user, us)).map(|(u, conv)| json!({ "user": u, "conv": conv })).collect();
    if let Some((target, conv)) = c.agent_busy.lock().unwrap_or_else(|e| e.into_inner()).clone() { if c.agent_enabled(us) && (target.is_empty() || target == user) { out.push(json!({ "user": AGENT, "conv": if target.is_empty() { if conv.is_empty() { "room".to_string() } else { conv } } else { let mut v = [AGENT.to_string(), user.to_string()]; v.sort(); format!("dm:{}|{}", v[0], v[1]) } })); } }
    out
}

// ---------- the status (plans/presence.md): presence × working / accepting work, the label, what a person set ----------
fn manual_of(hub: &dyn ChatHost, u: &str) -> Manual {
    let c = hub.chat();
    if let Some(m) = c.manual.lock().unwrap_or_else(|e| e.into_inner()).get(u).cloned() { return m; }
    let m = hub.account(u).and_then(|a| read_json(&a.dir.join("state.json"))).map(|st| Manual::from_state(&st)).unwrap_or_default();
    c.manual.lock().unwrap_or_else(|e| e.into_inner()).insert(u.to_string(), m.clone());
    m
}
/// One person's status as `viewer` sees it (themselves: with `real`, their actual presence).
fn status_of(hub: &dyn ChatHost, u: &str, viewer: &str, now: u64) -> Value {
    let c = hub.chat();
    let m = manual_of(hub, u).now(now);
    let (real, seen, devices) = c.real_presence(u, now);
    let shown = if m.over == "offline" { "offline" } else if m.over == "away" && real != "offline" { "away" } else { real };
    let working = hub.working(u);
    let label = label_of(shown, &m.over, working, m.accepting);
    let since = { let mut l = c.labels.lock().unwrap_or_else(|e| e.into_inner()); let e = l.entry(u.to_string()).or_insert_with(|| (label.to_string(), now)); if e.0 != label { *e = (label.to_string(), now); } e.1 };
    let hidden = m.over == "offline"; // appear offline: nothing else is shown
    let mut v = json!({ "user": u, "presence": shown, "label": label, "working": working && !hidden, "workingSessions": if hidden || !working { 0 } else { hub.working_count(u) }, "acceptingWork": m.accepting && !hidden, "dnd": m.dnd && !hidden,
        "text": if hidden { String::new() } else { m.text.clone() }, "since": since, "seen": if hidden { 0 } else { seen }, "devices": if hidden { 0 } else { devices } });
    if viewer == u { v["real"] = json!(real); v["override"] = json!(m.over); v["overrideUntil"] = json!(m.over_until); v["dndUntil"] = json!(m.dnd_until); v["textUntil"] = json!(m.text_until); v["hideWhere"] = json!(m.hide_where); if hidden { v["text"] = json!(m.text); } }
    v
}
/// Everyone's status (every account), as `viewer` sees it.
fn status_list(hub: &dyn ChatHost, viewer: &str, us: &[(String, String, bool)]) -> Vec<Value> {
    let now = now_ms(); let mut names: Vec<&String> = us.iter().map(|x| &x.0).collect(); names.sort();
    names.into_iter().map(|u| status_of(hub, u, viewer, now)).collect()
}
/// POST /api/presence/me: what a person sets about their presence; Err = 400 with why.
fn set_manual(hub: &dyn ChatHost, a: &Account, b: &Value) -> Result<bool, String> {
    let now = now_ms();
    let until = |k: &str| -> Result<Option<u64>, String> { match b.get(k) { None => Ok(None), Some(Value::Null) => Ok(Some(0)), Some(v) => match v.as_f64() { Some(x) if x == 0.0 => Ok(Some(0)), Some(x) if x > now as f64 => Ok(Some(x as u64)), _ => Err(format!("{k} must be a time in the future (ms), or null.")) } } };
    let flag = |k: &str| -> Result<Option<bool>, String> { match b.get(k) { None => Ok(None), Some(Value::Bool(x)) => Ok(Some(*x)), Some(_) => Err(format!("{k} must be true or false.")) } };
    let over = match b.get("override") { None => None, Some(v) => match v.as_str() { Some(x @ ("auto" | "away" | "offline")) => Some(x.to_string()), _ => return Err("override must be \"auto\", \"away\" or \"offline\".".into()) } };
    let text = match b.get("text") { None => None, Some(Value::Null) => Some(String::new()), Some(Value::String(t)) => { let t: String = t.chars().filter(|c| !c.is_control() || *c == ' ').collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "); if t.chars().count() > 80 { return Err("The status text can be up to 80 characters.".into()); } Some(t) } Some(_) => return Err("text must be a string.".into()) };
    let (over_until, dnd_until, text_until) = (until("overrideUntil")?, until("dndUntil")?, until("textUntil")?);
    let (dnd, accepting, hide_where) = (flag("dnd")?, flag("acceptingWork")?, flag("hideWhere")?);
    let f = a.dir.join("state.json");
    let mut st = read_json(&f).filter(Value::is_object).unwrap_or(json!({}));
    if !st["presence"].is_object() { st["presence"] = json!({}); }
    { let p = &mut st["presence"];
      if let Some(o) = &over { p["override"] = json!(o); p["overrideUntil"] = json!(over_until.unwrap_or(0)); } else if let Some(t) = over_until { p["overrideUntil"] = json!(t); }
      if let Some(d) = dnd { p["dnd"] = json!(d); p["dndUntil"] = json!(dnd_until.unwrap_or(0)); } else if let Some(t) = dnd_until { p["dndUntil"] = json!(t); }
      if let Some(x) = accepting { p["acceptingWork"] = json!(x); }
      if let Some(x) = hide_where { p["hideWhere"] = json!(x); } }
    let text_changed = text.is_some() || text_until.is_some();
    if text_changed {
        if !st["profile"].is_object() { st["profile"] = json!({}); }
        if let Some(t) = &text { st["profile"]["status"] = json!(t); st["profile"]["textUntil"] = json!(text_until.unwrap_or(0)); } else if let Some(t) = text_until { st["profile"]["textUntil"] = json!(t); }
    }
    write_json(&f, &st);
    hub.chat().manual.lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone(), Manual::from_state(&st));
    // an until that passes changes what others see: their pages hear it then
    for t in [over_until, dnd_until, text_until].into_iter().flatten().filter(|t| *t > now) { let c = hub.chat().clone(); tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(t - now + 50)).await; c.room_changed(); }); }
    Ok(text_changed)
}

// ---------- Unity: what she reads, what she posts ----------
impl Chat {
    fn agent_open(&self, m: &Value) -> Option<String> {
        let slot = &m["keys"][AGENT];
        if !slot.is_object() || m["deleted"] == json!(true) { return None; }
        let inc = Incoming { v: m["v"].as_u64().map(|v| v as u8), mid: m["mid"].as_str().map(String::from), from: s(m, "from"), to: m["to"].as_str().map(String::from), iv: s(m, "iv"), ct: s(m, "ct"), key: Wrapped { iv: s(slot, "iv"), wk: s(slot, "wk") }, from_pub: serde_json::from_value(m["fromPub"].clone()).ok()? };
        crate::e2e::open(&self.agent_key, AGENT, &inc).ok()
    }
    fn agent_read(&self, m: &Value) -> Option<String> { self.agent_open(m).map(|x| payload_text(&x)) }
    fn agent_context(&self, m: &Value) -> Vec<(String, String)> {
        let Some(x) = self.agent_open(m).filter(|x| x.starts_with('{')) else { return vec![] };
        let Ok(p) = serde_json::from_str::<Value>(&x) else { return vec![] };
        let ctx: Vec<Value> = p["ctx"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| c["t"].is_string()).collect();
        ctx.iter().skip(ctx.len().saturating_sub(5)).map(|c| (guilds::take_units(&if c["n"].is_null() { "?".to_string() } else { match &c["n"] { Value::String(t) => if t.is_empty() { "?".into() } else { t.clone() }, v => v.to_string() } }, 40), guilds::take_units(&s(c, "t"), 2000))).collect()
    }
}
fn payload_text(x: &str) -> String {
    if x.starts_with('{') { if let Ok(p) = serde_json::from_str::<Value>(x) { if p["v"] == json!(2) && p["t"].is_string() {
        let files = p["f"].as_array().cloned().unwrap_or_default();
        let att = if files.is_empty() { String::new() } else { format!(" (attached: {})", files.iter().map(|f| guilds::take_units(f["n"].as_str().filter(|n| !n.is_empty()).unwrap_or("a file"), 80)).collect::<Vec<_>>().join(", ")) };
        let t = s(&p, "t");
        let body = if p["a"].is_null() || p["a"] == json!(false) || p["a"] == json!(0) || p["a"] == json!("") { t } else { format!("{}(sent a voice clip)", if t.is_empty() { String::new() } else { format!("{t} ") }) };
        return body + &att;
    } } }
    x.to_string()
}
/// Posts as Unity (or with `as`, a Work run's "<person> · Claude" / "VibeRoller"), sealed with her key for every member.
pub fn agent_say(hub: &dyn ChatHost, text: &str, to: &str, as_: &str, via: &str, conv: &str) -> i64 {
    let c = &hub.chat(); let us = users(hub);
    let conv = if to.is_empty() && guilds::is_guild_conv(conv) { conv.to_string() } else { String::new() };
    let allowed = if conv.is_empty() { None } else { let ctx = ctx!(hub, us.clone()); Some(c.guilds.members(&conv, &ctx).unwrap_or_default()) };
    let mid = crate::e2e::new_mid();
    let aad = crate::e2e::body_aad(AGENT, to, &mid, 3);
    let raw: [u8; 32] = pgm_core_internal::crypto::random_bytes(32).try_into().expect("32");
    let iv = pgm_core_internal::crypto::random_bytes(12);
    let Ok(ct) = pgm_core_internal::crypto::aes_gcm_seal(&raw, &iv, aad.as_bytes(), &crate::e2e::pad_plain(text)) else { return 0 };
    let mut readers: Vec<(String, Value)> = vec![(AGENT.into(), c.agent_pub.clone())];
    { let keys = c.keys.lock().unwrap_or_else(|e| e.into_inner()); for (u, _, _) in &us { if let Some(k) = keys.get(u) { if (to.is_empty() || u == to) && allowed.as_ref().map(|l| l.contains(u)).unwrap_or(true) { readers.push((u.clone(), k["pub"].clone())); } } } }
    let mut slots = Map::new();
    for (u, p) in readers {
        let Ok(pj) = serde_json::from_value::<PubJwk>(p) else { continue };
        let Ok(wk) = c.agent_key.wrap_key(&pj) else { continue };
        let kiv = pgm_core_internal::crypto::random_bytes(12);
        if let Ok(w) = pgm_core_internal::crypto::aes_gcm_seal(&wk, &kiv, format!("{aad}|{u}").as_bytes(), &raw) { slots.insert(u, json!({ "iv": pgm_core_internal::b64(&kiv), "wk": pgm_core_internal::b64(&w) })); }
    }
    let seq = { let mut st = c.store.lock().unwrap_or_else(|e| e.into_inner()); st.seq += 1; st.seq };
    let mut msg = json!({ "seq": seq, "v": 3, "mid": mid, "from": AGENT });
    if !to.is_empty() { msg["to"] = json!(to); }
    if !conv.is_empty() { msg["conv"] = json!(conv); }
    for (k, v) in [("ts", json!(iso())), ("fromPub", c.agent_pub.clone()), ("iv", json!(pgm_core_internal::b64(&iv))), ("ct", json!(pgm_core_internal::b64(&ct))), ("keys", Value::Object(slots))] { msg[k] = v; }
    if !as_.is_empty() { msg["as"] = json!(guilds::take_units(as_, 60)); }
    if !via.is_empty() && us.iter().any(|u| u.0 == via) { msg["via"] = json!(via); }
    c.append(msg)
}
/// A message just in: is Unity called? (a private message to her, or @<her name> where she may read it)
async fn agent_hear(hub: &dyn ChatHost, msg: &Value) {
    let c = &hub.chat(); let us = users(hub);
    if !c.agent_enabled(&us) || msg["from"] == json!(AGENT) { return; }
    let off = hub.root().join("ai-host.off").exists();
    let off_note = |to: &str, conv: &str| { agent_say(hub, "I can't answer right now: my host paused the AI. Ask me again once it is back.", to, "", "", conv); };
    let from = s(msg, "from");
    let call = |target: String, caller: String, conv: String| hub.agent_called(target, caller, conv);
    if msg["to"] == json!(AGENT) { if c.agent_read(msg).is_some() { if off { return off_note(&from, ""); } call(from.clone(), from, String::new()); } return; }
    if !s(msg, "to").is_empty() { return; }
    let Some(text) = c.agent_read(msg) else { return };
    let low = text.to_lowercase(); let h = format!("@{}", c.handle);
    let called = low.match_indices(&h).any(|(i, _)| { let after = low[i + h.len()..].chars().next(); !after.map(|ch| ch.is_alphanumeric() || ch == '_').unwrap_or(false) });
    if called { if off { return off_note("", msg["conv"].as_str().unwrap_or("")); } call(String::new(), from, s(msg, "conv")); }
}
/// Where an answer to this message of hers goes: (private answer to, channel / group); ("", "") = # Everyone.
pub fn agent_dest(m: &Value) -> (String, String) { if m["to"] == json!(AGENT) { (s(m, "from"), String::new()) } else { (String::new(), s(m, "conv")) } }
/// What Unity was sent in one conversation since she last answered there (with the few messages before each call):
/// her brain's prompt. Like any agent's queue: every call in that conversation that came while she was busy is in this
/// one turn; other conversations' calls stay for their own turn and never show up here.
pub fn agent_blocks(hub: &dyn ChatHost, target: &str, conv: &str) -> String {
    let c = &hub.chat();
    let mut seen = c.agent_seen.lock().unwrap_or_else(|e| e.into_inner());
    let st = c.store.lock().unwrap_or_else(|e| e.into_inner());
    let open: Vec<(i64, String, &Value)> = st.messages.iter().filter(|m| { let q = m["seq"].as_i64().unwrap_or(0); q > seen.0 && !seen.1.contains(&q) && m["from"] != json!(AGENT) })
        .filter_map(|m| c.agent_read(m).map(|t| (m["seq"].as_i64().unwrap_or(0), t, m))).collect();
    let mine: Vec<&(i64, String, &Value)> = open.iter().filter(|(_, _, m)| { let (t, cv) = agent_dest(m); t == target && (!t.is_empty() || cv == conv) }).collect();
    let blocks: Vec<String> = mine.iter().skip(mine.len().saturating_sub(6)).map(|(_, t, m)| {
        let ctx: Vec<String> = c.agent_context(m).into_iter().map(|(n, t)| format!("  {n}: {t}")).collect();
        format!("{}[{}] {}{}: {}", if ctx.is_empty() { String::new() } else { format!("Earlier in that conversation:\n{}\n", ctx.join("\n")) }, s(m, "ts"), if m["to"] == json!(AGENT) { "(private message to you) " } else { "" }, hub.name_of(&s(m, "from")), t)
    }).collect();
    for (q, _, _) in &mine { seen.1.insert(*q); }
    // everything up to the first call still waiting for its turn is done
    let low = open.iter().map(|x| x.0).filter(|q| !seen.1.contains(q)).min().map(|q| q - 1).unwrap_or(st.seq);
    seen.0 = low.max(seen.0); let floor = seen.0; seen.1.retain(|q| *q > floor);
    write_json(&c.dir.join("agent-seen.json"), &json!({ "lastSeq": seen.0, "handled": seen.1.iter().collect::<Vec<_>>() }));
    if blocks.is_empty() { "(none)".into() } else { blocks.join("\n\n") }
}

// ---------- members + profiles (kept by the JavaScript hub in state.json; pictures in Avatars/) ----------
fn avatar(hub: &dyn ChatHost, u: &str) -> String {
    let c = &hub.chat();
    if let Some(a) = c.avatars.lock().unwrap_or_else(|e| e.into_inner()).get(u) { return a.clone(); }
    let mut url = String::new();
    for (ext, mime) in [("webp", "image/webp"), ("png", "image/png"), ("jpg", "image/jpeg")] { if let Ok(b) = std::fs::read(c.mills.join("Avatars").join(format!("{u}.{ext}"))) { url = format!("data:{mime};base64,{}", pgm_core_internal::b64(&b)); break; } }
    c.avatars.lock().unwrap_or_else(|e| e.into_inner()).insert(u.to_string(), url.clone()); url
}
pub fn public_prefs_of(hub: &dyn ChatHost, a: &Account) -> Value { public_prefs(hub, a) }
fn public_prefs(hub: &dyn ChatHost, a: &Account) -> Value {
    let st = read_json(&a.dir.join("state.json")).unwrap_or(json!({}));
    let p = &st["profile"];
    let status = if p["textUntil"].as_u64().map(|t| t > 0 && now_ms() >= t).unwrap_or(false) { String::new() } else { s(p, "status") }; // its until passed (plans/presence.md)
    json!({ "nick": st["prefs"]["nick"].as_str().unwrap_or(""), "color": if st["prefs"]["color"].is_null() { Value::Null } else { st["prefs"]["color"].clone() },
        "profile": { "bio": s(p, "bio"), "pronouns": s(p, "pronouns"), "status": status, "location": s(p, "location"), "links": if p["links"].is_array() { p["links"].clone() } else { json!([]) }, "accent": if p["accent"].is_null() { Value::Null } else { p["accent"].clone() }, "timezone": s(p, "timezone"), "avatar": avatar(hub, &a.username) } })
}

// ---------- calls ----------
impl Chat {
    fn call_wake(&self, users: &[String]) { let w = self.call_woke.lock().unwrap_or_else(|e| e.into_inner()); for u in users { if let Some(n) = w.get(u) { n.notify_waiters(); } } }
    fn call_notify(&self, u: &str) -> Arc<Notify> { self.call_woke.lock().unwrap_or_else(|e| e.into_inner()).entry(u.to_string()).or_default().clone() }
}
fn calls_changed(hub: &dyn ChatHost, conv: &str, us: &[(String, String, bool)]) { let c = &hub.chat(); c.calls.lock().unwrap_or_else(|e| e.into_inner()).ver += 1; let who: Vec<String> = us.iter().map(|x| x.0.clone()).filter(|u| conv_member(hub, conv, u, us)).collect(); c.call_wake(&who); }
pub fn calls_state(hub: &dyn ChatHost, user: &str, us: &[(String, String, bool)]) -> Value {
    let c = &hub.chat(); let list = c.calls.lock().unwrap_or_else(|e| e.into_inner()).calls.clone();
    let ctx = ctx!(hub, us.to_vec());
    let mut out = Map::new();
    for (conv, ps) in list { if ps.is_empty() || !conv_member(hub, &conv, user, us) { continue; }
        out.insert(conv.clone(), json!(ps.iter().map(|(u, p)| { let md = c.guilds.media(&conv, u, &ctx); let held = md["speak"] != json!(true) || md["muted"] == json!(true); let stream = md["stream"] == json!(true);
            let mut v = json!({ "user": u, "cam": p["cam"] == json!(true) && stream, "mic": p["mic"] == json!(true) && !held, "screen": p["screen"] == json!(true) && stream, "since": p["since"] });
            if held { v["held"] = json!(if md["muted"] == json!(true) { "muted" } else { "nospeak" }); } if !stream { v["nostream"] = json!(true); } v }).collect::<Vec<_>>())); }
    Value::Object(out)
}
pub fn calls_leave(hub: &dyn ChatHost, user: &str) {
    let us = users(hub); let mut changed = vec![];
    { let mut cs = hub.chat().calls.lock().unwrap_or_else(|e| e.into_inner()); for (c, ps) in cs.calls.iter_mut() { let n = ps.len(); ps.retain(|(u, _)| u != user); if ps.len() != n { changed.push(c.clone()); } } cs.calls.retain(|(_, ps)| !ps.is_empty()); cs.mailbox.remove(user); }
    for c in changed { calls_changed(hub, &c, &us); }
}
/// The chat's background work: calls nobody polls for a minute end, temporary members who went offline leave, receipts
/// are saved, a rolled-back deploy is announced.
pub fn start(hub: Arc<dyn ChatHost>) {
    // browsers long-poll at least every 25 s while in a call; 60 s of silence = gone
    let h = hub.clone();
    tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(10)).await; let now = now_ms(); let mut changed = vec![];
        { let mut cs = h.chat().calls.lock().unwrap_or_else(|e| e.into_inner()); for (c, ps) in cs.calls.iter_mut() { let n = ps.len(); ps.retain(|(_, p)| now.saturating_sub(p["seen"].as_u64().unwrap_or(0)) <= 60000); if ps.len() != n { changed.push(c.clone()); } } cs.calls.retain(|(_, ps)| !ps.is_empty()); }
        if !changed.is_empty() { let us = users(&*h); for c in changed { calls_changed(&*h, &c, &us); } } } });
    // temporary members who went offline leave; ended timeouts go
    let h = hub.clone();
    tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(20)).await; let us = users(&*h); let on = h.chat().online(&us); if h.chat().guilds.sweep(&|u| on.iter().any(|x| x == u)) { h.chat().room_changed(); } } });
    // receipts are saved half a second after a change
    let h = hub.clone();
    tokio::spawn(async move { loop { h.chat().seen_dirty.notified().await; tokio::time::sleep(Duration::from_millis(500)).await; let v = Value::Object(h.chat().seen.lock().unwrap_or_else(|e| e.into_inner()).clone()); write_json(&h.chat().dir.join("seen.json"), &v); } });
    // what the machine has to say to the team, posted as VibeRoller (every 10 s, the first time after 3 s): the
    // auto-deploy went back to the last good version (.rolled-back), the supervisor fell back to an earlier build
    // (.fell-back, FR4), notes left by the deploy or the supervisor (.announce/*.txt), and this program's own notices
    // (pgm-core notices: a data file that could not be read and was moved aside, FR2)
    let h = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            let root = h.root();
            let f = root.join(".rolled-back");
            if f.exists() {
                let text = std::fs::read_to_string(&f).unwrap_or_default().trim().to_string(); let _ = std::fs::remove_file(&f);
                agent_say(&*h, &format!("**VibeRoller rolled itself back.** {} Nothing was lost: deploys never touch data. Whoever shipped it: look at the deploy log, fix, and ship again.", if text.is_empty() { "A new version did not come up, so the hub went back to the last good one." } else { &text }), "", "VibeRoller", "", "");
            }
            let f = root.join(".fell-back");
            if let Some(v) = read_json(&f) {
                let _ = std::fs::remove_file(&f);
                let (from, to, why) = (s(&v, "from"), s(&v, "to"), s(&v, "why"));
                agent_say(&*h, &format!("**VibeRoller fell back to an earlier build.** The build {} {}, so the hub runs the previous good build {} now. Nothing was lost. Whoever shipped it: look at the server's log, fix, and ship again.", &from[..from.len().min(7)], if why.is_empty() { "failed" } else { &why }, &to[..to.len().min(7)]), "", "VibeRoller", "", "");
            }
            let dir = root.join(".announce");
            if let Ok(rd) = std::fs::read_dir(&dir) {
                let mut names: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map(|x| x == "txt").unwrap_or(false)).collect();
                names.sort();
                for p in names { let text = std::fs::read_to_string(&p).unwrap_or_default().trim().to_string(); let _ = std::fs::remove_file(&p); if !text.is_empty() { agent_say(&*h, &text, "", "VibeRoller", "", ""); } }
            }
            for text in pgm_core_internal::notices::drain() { agent_say(&*h, &text, "", "VibeRoller", "", ""); }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
}

/// The slots a message may carry: members (and Unity) only; a private message only its two people; a channel / group
/// only that conversation's members.
fn clean_slots(keys: &Value, from: &str, to: &str, allowed: Option<&[String]>, us: &[(String, String, bool)], agent: bool) -> Map<String, Value> {
    let mut out = Map::new();
    for (u, k) in keys.as_object().cloned().unwrap_or_default() {
        if !(us.iter().any(|x| x.0 == u) || (u == AGENT && agent)) { continue; }
        if !to.is_empty() && u != from && u != to { continue; }
        if let Some(l) = allowed { if u != AGENT && u != from && !l.contains(&u) { continue; } }
        if b64ok(&k["iv"], 64) && b64ok(&k["wk"], 200) { out.insert(u, json!({ "iv": k["iv"], "wk": k["wk"] })); }
    }
    out
}
fn valid_jwk(k: &Value) -> bool { k.is_object() && k["kty"] == json!("EC") && k["crv"] == json!("P-256") && b64ok(&k["x"], 64) && b64ok(&k["y"], 64) && k.get("d").is_none() }
fn mid_ok(m: &Value) -> bool { m.as_str().map(|x| x.len() == 22 && x.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')).unwrap_or(false) }

/// The chat's routes (signed in as `a`); None = not one of them.
pub async fn route(hub: &dyn ChatHost, r: &Req, a: &Account) -> Option<Resp> {
    let c = &hub.chat(); let u = a.username.as_str();
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let p = r.path.as_str();
    if !(p.starts_with("/api/room/") || p == "/api/presence" || p == "/api/presence/me" || p.starts_with("/api/call/") || p == "/api/guilds" || p.starts_with("/api/guilds/") || p == "/api/admin/room/clear") { return None; }
    let us = users(hub); let agent_on = c.agent_enabled(&us);
    let b = r.json();
    Some(match p {
        "/api/room/members" if get => {
            let keys = c.keys.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let mut list: Vec<Value> = hub.accounts().iter().map(|acc| { let pp = public_prefs(hub, acc); json!({ "username": acc.username, "display": acc.display, "nick": pp["nick"], "color": pp["color"], "profile": pp["profile"], "pub": keys.get(&acc.username).map(|k| k["pub"].clone()).unwrap_or(Value::Null) }) }).collect();
            if agent_on { list.push(json!({ "username": AGENT, "display": c.agent_name, "nick": "", "color": Value::Null, "pub": c.agent_pub, "agent": true })); }
            answer(200, json!({ "members": list }))
        }
        "/api/room/mykey" if get => answer(200, c.keys.lock().unwrap_or_else(|e| e.into_inner()).get(u).cloned().unwrap_or(json!({}))),
        "/api/room/mykey" if post => {
            let (pubk, backup) = (&b["pub"], &b["backup"]);
            if !valid_jwk(pubk) || !backup.is_object() || !b64ok(&backup["salt"], 64) || !b64ok(&backup["iv"], 64) || !b64ok(&backup["ct"], 4096) { return Some(answer(400, json!({ "error": "Bad key." }))); }
            c.keys.lock().unwrap_or_else(|e| e.into_inner()).insert(u.into(), json!({ "pub": { "kty": "EC", "crv": "P-256", "x": pubk["x"], "y": pubk["y"] }, "backup": { "salt": backup["salt"], "iv": backup["iv"], "ct": backup["ct"] }, "updated": iso() }));
            c.save_keys(); c.keys_changed();
            answer(200, json!({ "ok": true }))
        }
        "/api/room/send" if post => {
            let to = s(&b, "to"); let conv_in = s(&b, "conv"); let conv = if !conv_in.is_empty() && conv_in != "room" { conv_in } else { String::new() };
            let ctx = ctx!(hub, us.clone());
            if !conv.is_empty() && (!to.is_empty() || !guilds::is_guild_conv(&conv) || !c.guilds.is_member(&conv, u, &ctx)) { return Some(answer(400, json!({ "error": "Unknown conversation." }))); }
            let why = if to.is_empty() { c.guilds.post_check(if conv.is_empty() { "room" } else { &conv }, u, b.get("blob").is_some(), false, &ctx) } else { String::new() };
            if !why.is_empty() { return Some(answer(403, json!({ "error": why }))); }
            let v = &b["v"];
            if !v.is_null() && !(matches!(v.as_u64(), Some(3 | 4)) && mid_ok(&b["mid"]) && !c.store.lock().unwrap_or_else(|e| e.into_inner()).messages.iter().any(|x| x["mid"] == b["mid"])) { return Some(answer(400, json!({ "error": "Bad message." }))); }
            if b.get("blob").is_some() && !(v.as_u64() == Some(4) && b64ok(&b["blob"], MAX_CT)) { return Some(answer(if b["blob"].as_str().map(|x| x.len() > MAX_CT).unwrap_or(false) { 413 } else { 400 }, json!({ "error": "That voice clip is too large or malformed (up to about 2 minutes)." }))); }
            let Some(mine) = c.pub_of(u) else { return Some(answer(400, json!({ "error": "Your chat key is not set up yet." }))) };
            if b["ct"].as_str().map(|x| x.len() > MAX_CT).unwrap_or(false) { return Some(answer(413, json!({ "error": "That message is too large (voice clips can be up to about 2 minutes)." }))); }
            if !b64ok(&b["iv"], 64) || !b64ok(&b["ct"], MAX_CT) || !b["keys"].is_object() { return Some(answer(400, json!({ "error": "Bad message." }))); }
            if b.get("to").is_some() && !to.is_empty() && (to == u || !(us.iter().any(|x| x.0 == to) || (to == AGENT && agent_on))) { return Some(answer(400, json!({ "error": "Unknown recipient." }))); }
            let allowed = if conv.is_empty() { None } else { Some(c.guilds.members(&conv, &ctx).unwrap_or_default()) };
            let slots = clean_slots(&b["keys"], u, &to, allowed.as_deref(), &us, agent_on);
            if !slots.contains_key(u) { return Some(answer(400, json!({ "error": "Bad message." }))); }
            // its files (uploaded encrypted just before by this sender) now belong to this message: its number is taken first
            let file_conv = if !to.is_empty() { let mut v = [u.to_string(), to.clone()]; v.sort(); format!("dm:{}|{}", v[0], v[1]) } else if conv.is_empty() { "room".into() } else { conv.clone() };
            let seqn = { let mut st = c.store.lock().unwrap_or_else(|e| e.into_inner()); st.seq += 1; st.seq };
            let file_ids = if b.get("files").is_some() { match hub.files().attach(Some(a), u, &b["files"], seqn, &file_conv) { Ok(ids) => ids, Err(f) => return Some(answer(f.status, json!({ "error": f.msg }))) } } else { vec![] };
            let mut msg = json!({ "seq": seqn });
            if !v.is_null() { msg["v"] = v.clone(); msg["mid"] = b["mid"].clone(); }
            msg["from"] = json!(u); if !to.is_empty() { msg["to"] = json!(to); } if !conv.is_empty() { msg["conv"] = json!(conv); }
            for (k, val) in [("ts", json!(iso())), ("fromPub", mine), ("iv", b["iv"].clone()), ("ct", b["ct"].clone()), ("keys", Value::Object(slots))] { msg[k] = val; }
            if b["blob"].as_str().map(|x| !x.is_empty()).unwrap_or(false) { msg["blob"] = b["blob"].clone(); }
            if !file_ids.is_empty() { msg["files"] = json!(file_ids); }
            c.append(msg.clone());
            if to.is_empty() { c.guilds.note_post_by(if conv.is_empty() { "room" } else { &conv }, u); }
            agent_hear(hub, &msg).await;
            answer(200, json!({ "seq": seqn }))
        }
        "/api/room/edit" if post => {
            let target = b["seq"].as_i64().unwrap_or(-1);
            let m = c.store.lock().unwrap_or_else(|e| e.into_inner()).messages.iter().find(|x| x["seq"].as_i64() == Some(target)).cloned();
            let Some(m) = m.filter(|m| m["from"] == json!(u) && m["deleted"] != json!(true)) else { return Some(answer(403, json!({ "error": "You can only edit your own messages." }))) };
            let ctx = ctx!(hub, us.clone());
            let why = if s(&m, "to").is_empty() { c.guilds.post_check(m["conv"].as_str().filter(|x| !x.is_empty()).unwrap_or("room"), u, false, true, &ctx) } else { String::new() };
            if !why.is_empty() { return Some(answer(403, json!({ "error": why }))); }
            let Some(mine) = c.pub_of(u).filter(|_| b64ok(&b["iv"], 64) && b64ok(&b["ct"], MAX_CT)) else { return Some(answer(400, json!({ "error": "Bad message." }))) };
            let v = &b["v"];
            if !v.is_null() && !(matches!(v.as_u64(), Some(3 | 4)) && mid_ok(&b["mid"]) && (if m["mid"].is_string() { m["mid"] == b["mid"] } else { !c.store.lock().unwrap_or_else(|e| e.into_inner()).messages.iter().any(|x| x["mid"] == b["mid"]) })) { return Some(answer(400, json!({ "error": "Bad message." }))); }
            if m["blob"].is_string() && v.as_u64() != Some(4) { return Some(answer(400, json!({ "error": "Bad message." }))); }
            let conv = s(&m, "conv"); let allowed = if conv.is_empty() { None } else { c.guilds.members(&conv, &ctx) };
            let slots = clean_slots(&b["keys"], u, &s(&m, "to"), allowed.as_deref(), &us, agent_on);
            if !slots.contains_key(u) { return Some(answer(400, json!({ "error": "Bad message." }))); }
            let rev = { let mut st = c.store.lock().unwrap_or_else(|e| e.into_inner()); st.rev += 1; let rev = st.rev;
                if let Some(x) = st.messages.iter_mut().find(|x| x["seq"].as_i64() == Some(target)) {
                    if !v.is_null() { x["v"] = v.clone(); x["mid"] = b["mid"].clone(); } else if let Some(o) = x.as_object_mut() { o.remove("v"); o.remove("mid"); }
                    for (k, val) in [("iv", b["iv"].clone()), ("ct", b["ct"].clone()), ("keys", Value::Object(slots)), ("fromPub", mine), ("editedAt", json!(iso())), ("rev", json!(rev))] { x[k] = val; }
                }
                Chat::save_messages(&st, &c.dir); rev };
            c.room_changed();
            answer(200, json!({ "seq": target, "rev": rev }))
        }
        "/api/room/delete" if post => {
            let target = b["seq"].as_i64().unwrap_or(-1);
            let m = c.store.lock().unwrap_or_else(|e| e.into_inner()).messages.iter().find(|x| x["seq"].as_i64() == Some(target)).cloned();
            let ok = match &m { None => false, Some(m) => m["from"] == json!(u) || (s(m, "to").is_empty() && { let ctx = ctx!(hub, us.clone()); c.guilds.moderates(m["conv"].as_str().filter(|x| !x.is_empty()).unwrap_or("room"), u, &ctx) } && reader(hub, u, &us).visible(m)) };
            if !ok { return Some(answer(403, json!({ "error": "You can only delete your own messages." }))); }
            let mut gone = vec![];
            let rev = { let mut st = c.store.lock().unwrap_or_else(|e| e.into_inner()); let mut rev = st.rev; let mut changed = false;
                if let Some(x) = st.messages.iter_mut().find(|x| x["seq"].as_i64() == Some(target)) { if x["deleted"] != json!(true) { tombstone(x, &mut gone); rev += 1; x["rev"] = json!(rev); changed = true; } else { rev = x["rev"].as_i64().unwrap_or(rev); } }
                if changed { st.rev = rev; Chat::save_messages(&st, &c.dir); } rev };
            if !gone.is_empty() { hub.files().message_gone(&gone, &|x| hub.account(x)); }
            c.room_changed();
            answer(200, json!({ "seq": target, "rev": rev }))
        }
        "/api/room/typing" | "/api/room/seen" if post => {
            let conv = s(&b, "conv"); let ctx = ctx!(hub, us.clone());
            // (a private conversation: two real accounts in order, this person one of them; nothing made up is kept)
            let known = |n: &String| n == AGENT || us.iter().any(|q| q.0 == *n);
            let dm = dm_of(&conv).filter(|(x, y)| x < y && known(x) && known(y));
            if conv != "room" && !dm.as_ref().map(|(x, y)| x == u || y == u).unwrap_or(false) && !c.guilds.is_member(&conv, u, &ctx) { return Some(answer(400, json!({ "error": "Unknown conversation." }))); }
            if p == "/api/room/typing" {
                let on = !(b["on"].is_null() || b["on"] == json!(false) || b["on"] == json!(0) || b["on"] == json!(""));
                let may = dm.is_some() || c.guilds.post_check(&conv, u, false, true, &ctx).is_empty();
                c.set_typing(u, &conv, on && may);
            } else {
                let v = (b["seq"].as_f64().unwrap_or(0.0) as i64).min(c.seq());
                let changed = { let mut seen = c.seen.lock().unwrap_or_else(|e| e.into_inner()); let mine = seen.entry(u.to_string()).or_insert(json!({})); if v > mine[&conv].as_i64().unwrap_or(0) { mine[&conv] = json!(v); true } else { false } };
                if changed { c.seen_dirty.notify_one(); c.room_changed(); }
            }
            answer(200, json!({ "ok": true }))
        }
        "/api/room/messages" if get => {
            if r.param("wait").as_deref() != Some("1") { c.touch(u); return Some(room_answer(hub, r, u, &us)); }
            // judged before this page's own coming online moves the version; listening starts after it
            let stale = r.param("ver").and_then(|x| x.parse::<u64>().ok()) != Some(c.room_ver.load(Ordering::SeqCst));
            let dev = device_id(r.param("device").as_deref());
            c.poll_opened(u, &dev);
            let woke = c.room_woke.notified(); tokio::pin!(woke); woke.as_mut().enable();
            struct Open(Arc<Chat>, String, String);
            impl Drop for Open { fn drop(&mut self) { self.0.poll_closed(&self.1, &self.2); } }
            let chat_arc = hub.chat().clone();
            let guard = Open(chat_arc, u.to_string(), dev);
            if !stale { let _ = tokio::time::timeout(Duration::from_secs(25), woke).await; }
            drop(guard);
            room_answer(hub, r, u, &us)
        }
        "/api/presence" if post => {
            let truthy = |x: &Value| !(x.is_null() || *x == json!(false) || *x == json!(0) || *x == json!(""));
            let named = b["device"].as_str().map(|x| !x.trim().is_empty()).unwrap_or(false); let dev = device_id(b["device"].as_str());
            if truthy(&b["gone"]) { if named { c.device_gone(u, &dev); } else { c.gone(u); } } // an older page without a device id: everything, as before
            else { let screen = b["screen"].as_str().map(|x| guilds::take_units(x, 12)); let idle = b.get("idle").map(truthy); let locked = b.get("locked").map(truthy); c.pres_set(u, |pr| { if let Some(sc) = screen { pr.screen = sc; } let d = pr.dev(&dev); if let Some(i) = idle { d.idle = i; } if let Some(l) = locked { d.locked = l; } d.seen = now_ms(); }); }
            answer(200, json!({ "ok": true }))
        }
        "/api/presence" if get => answer(200, json!({ "people": status_list(hub, u, &us) })),
        "/api/presence/me" if get => answer(200, status_of(hub, u, u, now_ms())),
        "/api/presence/me" if post => match set_manual(hub, a, &b) {
            Ok(text_changed) => { if text_changed { c.keys_changed(); } else { c.room_changed(); } answer(200, status_of(hub, u, u, now_ms())) }
            Err(e) => answer(400, json!({ "error": e })),
        },
        "/api/room/blob" if get => {
            let n = r.param("seq").and_then(|x| x.parse::<i64>().ok()).unwrap_or(-1);
            let rd = reader(hub, u, &us);
            let st = c.store.lock().unwrap_or_else(|e| e.into_inner());
            match st.messages.iter().find(|m| m["seq"].as_i64() == Some(n)).filter(|m| m["blob"].is_string() && rd.visible(m) && !m["keys"][u].is_null()) { Some(m) => answer(200, json!({ "blob": m["blob"] })), None => answer(404, json!({ "error": "Not found." })) }
        }
        p if p.starts_with("/api/call/") => return Some(calls_route(hub, r, a, &us, &b).await),
        "/api/guilds" if get => { let ctx = ctx!(hub, us.clone()); answer(200, c.guilds.view(u, &ctx)) }
        p if post && p.starts_with("/api/guilds/") => match guild_op_name(p) {
            Some(op) => {
                let ctx = ctx!(hub, us.clone());
                let (res, ann) = c.guilds.op(op, u, &if b.is_object() { b.clone() } else { json!({}) }, &ctx);
                for (conv, text) in ann { agent_say(hub, &text, "", &c.guilds.home, "", &conv); }
                match res {
                    Ok(v) => { c.room_changed(); let view = c.guilds.view(u, &ctx); let mut out = json!({ "ok": true, "result": if v.is_object() && v.get("password").is_some() && v.get("members").is_some() { json!({ "id": v["id"], "name": v["name"] }) } else { v } }); for (k, x) in view.as_object().cloned().unwrap_or_default() { out[k] = x; } answer(200, out) }
                    Err(e) => answer(400, json!({ "error": e })),
                }
            }
            None => answer(404, json!({ "error": "Unknown endpoint" })),
        },
        "/api/admin/room/clear" if post => {
            if !a.admin { return Some(answer(403, json!({ "error": "Admins only." }))); }
            { let mut st = c.store.lock().unwrap_or_else(|e| e.into_inner()); st.messages.clear(); st.rev = 0; let _ = std::fs::write(c.dir.join("messages.jsonl"), ""); }
            hub.files().all_gone(&|x| hub.account(x));
            *c.agent_seen.lock().unwrap_or_else(|e| e.into_inner()) = (0, Default::default()); write_json(&c.dir.join("agent-seen.json"), &json!({ "lastSeq": 0 }));
            c.seen.lock().unwrap_or_else(|e| e.into_inner()).clear(); write_json(&c.dir.join("seen.json"), &json!({}));
            c.room_changed();
            println!("[vr-hub] {u} cleared the chat history.");
            answer(200, json!({ "ok": true }))
        }
        _ => return None,
    })
}
fn guild_op_name(p: &str) -> Option<&'static str> {
    Some(match p {
        "/api/guilds/server" => "createServer", "/api/guilds/server/update" => "updateServer", "/api/guilds/server/rename" => "renameServer", "/api/guilds/server/password" => "setPassword",
        "/api/guilds/server/transfer" => "transferServer", "/api/guilds/server/leave" => "leaveServer", "/api/guilds/server/delete" => "deleteServer",
        "/api/guilds/server/add" => "addMembers", "/api/guilds/server/invite" => "invitePeople", "/api/guilds/server/remove" => "removeMember",
        "/api/guilds/invitation/cancel" => "cancelInvitation", "/api/guilds/invitation/accept" => "acceptInvitation", "/api/guilds/invitation/decline" => "declineInvitation",
        "/api/guilds/member/kick" => "kick", "/api/guilds/member/ban" => "ban", "/api/guilds/member/unban" => "unban", "/api/guilds/member/nick" => "setNick", "/api/guilds/member/roles" => "setRoles",
        "/api/guilds/member/timeout" => "timeout", "/api/guilds/member/mute" => "voiceMute", "/api/guilds/member/selfrole" => "selfRole",
        "/api/guilds/role" => "createRole", "/api/guilds/role/update" => "updateRole", "/api/guilds/role/delete" => "deleteRole",
        "/api/guilds/invite" => "createInvite", "/api/guilds/invite/revoke" => "revokeInvite", "/api/guilds/invite/preview" => "previewInvite", "/api/guilds/invite/join" => "joinByCode",
        "/api/guilds/channel" => "createChannel", "/api/guilds/channel/update" => "updateChannel", "/api/guilds/channel/move" => "moveChannel", "/api/guilds/channel/delete" => "deleteChannel",
        "/api/guilds/pin" => "pin",
        "/api/guilds/group" => "createGroup", "/api/guilds/group/rename" => "renameGroup", "/api/guilds/group/add" => "addToGroup", "/api/guilds/group/remove" => "removeFromGroup",
        "/api/guilds/group/transfer" => "transferGroup", "/api/guilds/group/leave" => "leaveGroup",
        _ => return None,
    })
}
/// The room's answer: what changed for this reader, receipts, shares, versions, who is here, who is typing.
fn room_answer(hub: &dyn ChatHost, r: &Req, u: &str, us: &[(String, String, bool)]) -> Resp {
    let c = &hub.chat();
    let rd = reader(hub, u, us);
    let strip = |m: &Value| -> Value { let mut o = m.as_object().cloned().unwrap_or_default(); let key = o.remove("keys").map(|k| k[u].clone()).filter(|k| !k.is_null()).unwrap_or(Value::Null); let blob = o.remove("blob"); o.insert("key".into(), key); if let Some(bl) = blob.and_then(|b| b.as_str().map(|x| x.len())) { o.insert("blobLen".into(), json!(bl)); } Value::Object(o) };
    let (out, latest, latest_rev): (Vec<Value>, i64, i64) = {
        let st = c.store.lock().unwrap_or_else(|e| e.into_inner());
        let mine: Vec<&Value> = st.messages.iter().filter(|m| rd.visible(m)).collect();
        let pick: Vec<&Value> = if let Some(rv) = r.param("rev") {
            let after: i64 = rv.parse::<f64>().map(|f| f as i64).unwrap_or(0);
            if after != 0 { mine.into_iter().filter(|m| m["rev"].as_i64().or(m["seq"].as_i64()).unwrap_or(0) > after).collect() } else { let k = mine.len().saturating_sub(500); mine[k..].to_vec() }
        } else { let since: i64 = r.param("since").and_then(|x| x.parse::<f64>().ok()).map(|f| f as i64).unwrap_or(0); let l: Vec<&Value> = mine.into_iter().filter(|m| m["seq"].as_i64().unwrap_or(0) > since).collect(); let k = l.len().saturating_sub(300); l[k..].to_vec() };
        (pick.into_iter().map(strip).collect(), st.seq, st.rev)
    };
    let mut seen_out = Map::new();
    for (who, convs) in c.seen.lock().unwrap_or_else(|e| e.into_inner()).iter() { for (cv, v) in convs.as_object().cloned().unwrap_or_default() { if rd.sees_conv(&cv) { seen_out.entry(who.clone()).or_insert(json!({}))[&cv] = v; } } }
    answer(200, json!({ "messages": out, "latest": latest, "latestRev": latest_rev, "seen": seen_out, "shares": hub.shares_for(u), "ver": c.room_ver.load(Ordering::SeqCst), "keysVer": c.keys_ver.load(Ordering::SeqCst), "guildsVer": c.guilds.version(), "notesVer": hub.notes_version(),
        "status": status_list(hub, u, us), "online": c.online(us), "presence": c.presence_list(us), "typing": typing_for(hub, u, us) }))
}
async fn calls_route(hub: &dyn ChatHost, r: &Req, a: &Account, us: &[(String, String, bool)], b: &Value) -> Resp {
    let c = &hub.chat(); let u = a.username.as_str();
    let ctx = ctx!(hub, us.to_vec());
    let valid_conv = |conv: &str| -> bool { if conv == "room" { return true; } if let Some((x, y)) = dm_of(conv) { return x < y && us.iter().any(|q| q.0 == x) && us.iter().any(|q| q.0 == y); } guilds::is_guild_conv(conv) && c.guilds.exists(conv, &ctx) };
    let flag = |k: &str| !(b[k].is_null() || b[k] == json!(false) || b[k] == json!(0) || b[k] == json!(""));
    match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/api/call/join") => {
            let conv = s(b, "conv");
            if !valid_conv(&conv) || !conv_member(hub, &conv, u, us) { return answer(400, json!({ "error": "Unknown conversation." })); }
            if !c.has_key(u) { return answer(400, json!({ "error": "Your chat key is not set up yet." })); }
            if c.guilds.media(&conv, u, &ctx)["connect"] != json!(true) { return answer(403, json!({ "error": "You may not join calls here (no \"connect\" permission, or you are timed out)." })); }
            let mut left = vec![];
            { let mut cs = c.calls.lock().unwrap_or_else(|e| e.into_inner());
              for (cv, ps) in cs.calls.iter_mut() { if *cv != conv { let n = ps.len(); ps.retain(|(x, _)| x != u); if ps.len() != n { left.push(cv.clone()); } } }
              cs.calls.retain(|(_, ps)| !ps.is_empty());
              if !cs.calls.iter().any(|(cv, _)| *cv == conv) { cs.calls.push((conv.clone(), vec![])); }
              let ps = &mut cs.calls.iter_mut().find(|(cv, _)| *cv == conv).unwrap().1;
              let since = ps.iter().find(|(x, _)| x == u).map(|(_, p)| p["since"].clone()).unwrap_or(json!(iso()));
              ps.retain(|(x, _)| x != u); ps.push((u.to_string(), json!({ "cam": flag("cam"), "mic": flag("mic"), "screen": flag("screen"), "since": since, "seen": now_ms() }))); }
            for cv in left { calls_changed(hub, &cv, us); }
            calls_changed(hub, &conv, us);
            let ice = hub.cfg().get("iceServers").filter(|v| v.is_array()).cloned().unwrap_or_else(|| serde_json::from_str(ICE_DEFAULT).unwrap());
            answer(200, json!({ "calls": calls_state(hub, u, us), "ver": c.calls.lock().unwrap_or_else(|e| e.into_inner()).ver, "iceServers": ice }))
        }
        ("POST", "/api/call/leave") => { calls_leave(hub, u); answer(200, json!({ "ok": true })) }
        ("POST", "/api/call/state") => {
            let conv = s(b, "conv");
            let found = { let mut cs = c.calls.lock().unwrap_or_else(|e| e.into_inner()); match cs.calls.iter_mut().find(|(cv, _)| *cv == conv).and_then(|(_, ps)| ps.iter_mut().find(|(x, _)| x == u)) { Some((_, pv)) => { pv["cam"] = json!(flag("cam")); pv["mic"] = json!(flag("mic")); pv["screen"] = json!(flag("screen")); pv["seen"] = json!(now_ms()); true } None => false } };
            if !found { return answer(409, json!({ "error": "You are not in that call." })); }
            calls_changed(hub, &conv, us); answer(200, json!({ "ok": true }))
        }
        ("POST", "/api/call/signal") => {
            let (to, conv) = (s(b, "to"), s(b, "conv"));
            let both = { let cs = c.calls.lock().unwrap_or_else(|e| e.into_inner()); cs.calls.iter().find(|(cv, _)| *cv == conv).map(|(_, ps)| ps.iter().any(|(x, _)| x == u) && ps.iter().any(|(x, _)| *x == to)).unwrap_or(false) };
            if !both || to == u { return answer(409, json!({ "error": "Both people must be in this call." })); }
            if !b64ok(&b["iv"], 64) || !b64ok(&b["ct"], 200000) { return answer(400, json!({ "error": "Bad signal." })); }
            { let mut cs = c.calls.lock().unwrap_or_else(|e| e.into_inner()); cs.n += 1; let n = cs.n; let bx = cs.mailbox.entry(to.clone()).or_default(); bx.push(json!({ "n": n, "from": u, "conv": conv, "iv": b["iv"], "ct": b["ct"] })); if bx.len() > 500 { let k = bx.len() - 500; bx.drain(0..k); } }
            c.call_wake(&[to]); answer(200, json!({ "ok": true }))
        }
        ("GET", "/api/call/poll") => {
            let after: u64 = r.param("after").and_then(|x| x.parse::<f64>().ok()).map(|f| f as u64).unwrap_or(0); let ver: Option<u64> = r.param("ver").and_then(|x| x.parse().ok());
            { let mut cs = c.calls.lock().unwrap_or_else(|e| e.into_inner()); if let Some(bx) = cs.mailbox.get_mut(u) { bx.retain(|x| x["n"].as_u64().unwrap_or(0) > after); } }
            let woke = c.call_notify(u); let n = woke.notified(); tokio::pin!(n); n.as_mut().enable();
            let ready = { let cs = c.calls.lock().unwrap_or_else(|e| e.into_inner()); cs.mailbox.get(u).map(|bx| bx.iter().any(|x| x["n"].as_u64().unwrap_or(0) > after)).unwrap_or(false) || ver != Some(cs.ver) };
            if !ready { let _ = tokio::time::timeout(Duration::from_secs(25), n).await; }
            let (sigs, v) = { let mut cs = c.calls.lock().unwrap_or_else(|e| e.into_inner()); for (_, ps) in cs.calls.iter_mut() { if let Some((_, pv)) = ps.iter_mut().find(|(x, _)| x == u) { pv["seen"] = json!(now_ms()); } } (cs.mailbox.get(u).map(|bx| bx.iter().filter(|x| x["n"].as_u64().unwrap_or(0) > after).cloned().collect::<Vec<_>>()).unwrap_or_default(), cs.ver) };
            let last = sigs.last().and_then(|x| x["n"].as_u64()).unwrap_or(after);
            answer(200, json!({ "signals": sigs, "after": last, "calls": calls_state(hub, u, us), "ver": v }))
        }
        _ => answer(404, json!({ "error": "Unknown endpoint" })),
    }
}

/// The JavaScript hub asking (its Unity: post, read what she was sent, show her composing; its profiles changed).
pub fn front(hub: &dyn ChatHost, what: &str, b: &Value) -> Value {
    let c = &hub.chat();
    match what {
        "say" => json!({ "seq": agent_say(hub, &s(b, "text"), &s(b, "to"), &s(b, "as"), &s(b, "via"), &s(b, "conv")) }),
        "agent-prompt" => json!({ "blocks": agent_blocks(hub, &s(b, "target"), &s(b, "conv")) }),
        "agent-typing" => { *c.agent_busy.lock().unwrap_or_else(|e| e.into_inner()) = if b["on"] == json!(true) { Some((s(b, "target"), s(b, "conv"))) } else { None }; c.room_changed(); json!({ "ok": true }) }
        "has-key" => json!({ "ok": c.has_key(&s(b, "user")) }),
        "changed" => { if b["keys"] == json!(true) { c.keys_changed(); } else { c.room_changed(); } json!({ "ok": true }) }
        _ => json!({ "ok": false, "error": "unknown" }),
    }
}

/// The team tools of a Work run (bridge/team.js): post as "<person> · Claude", who is here, servers, the wider picture.
pub fn team_tool(hub: &dyn ChatHost, name: &str, args: &Value, a: &Account) -> Value {
    let c = &hub.chat(); let us = users(hub); let u = a.username.as_str();
    let ok = |v: Value| json!({ "ok": true, "result": v }); let no = |e: String| json!({ "ok": false, "error": e });
    let disp = |x: &str| hub.name_of(x);
    let now = now_ms() as i64;
    // (plans/presence.md: with the label, working / accepting work, do not disturb and the status text, so an AI can check
    // before it assigns or messages)
    let people = || -> Vec<Value> { c.presence_list(&us).into_iter().map(|p| { let seen = p["seen"].as_i64().unwrap_or(0); let st = status_of(hub, &s(&p, "user"), u, now as u64); json!({ "user": p["user"], "name": disp(&s(&p, "user")), "state": if p["online"] == json!(true) { if p["idle"] == json!(true) { "away" } else { "online" } } else { "offline" }, "label": st["label"], "working": st["working"], "acceptingWork": st["acceptingWork"], "dnd": st["dnd"], "text": st["text"], "screen": s(&p, "screen"), "lastSeen": if seen > 0 { json!(guilds_iso(seen)) } else { Value::Null }, "agoMin": if seen > 0 { json!(((now - seen) as f64 / 60000.0).round() as i64) } else { Value::Null } }) }).collect() };
    match name {
        "team_channel_create" | "team_group_create" => {
            let ctx = ctx!(hub, us.clone());
            let (op, body) = if name == "team_channel_create" { ("createChannel", json!({ "server": if s(args, "server").is_empty() { "mills".to_string() } else { s(args, "server") }, "name": s(args, "name"), "topic": s(args, "topic") })) }
                else { ("createGroup", json!({ "name": s(args, "name"), "members": if args["members"].is_array() { args["members"].clone() } else { json!(s(args, "members").split(|ch: char| ch == ',' || ch.is_whitespace()).filter(|x| !x.is_empty()).collect::<Vec<_>>()) } })) };
            let (res, ann) = c.guilds.op(op, u, &body, &ctx);
            for (conv, text) in ann { agent_say(hub, &text, "", &c.guilds.home, "", &conv); }
            match res {
                Ok(v) => { c.room_changed(); if op == "createChannel" { ok(json!({ "created": true, "conv": format!("ch:{}/{}", s(&body, "server"), s(&v, "id")), "channel": v })) } else { ok(json!({ "created": true, "conv": format!("grp:{}", s(&v, "id")), "group": { "id": v["id"], "name": v["name"], "members": v["members"] } })) } }
                Err(e) => no(e),
            }
        }
        "hub_status" => {
            let calls = calls_state(hub, u, &us);
            let in_calls: Vec<Value> = calls.as_object().cloned().unwrap_or_default().into_iter().map(|(conv, ps)| json!({ "conv": conv, "people": ps.as_array().cloned().unwrap_or_default().iter().map(|p| json!({ "user": p["user"], "name": disp(&s(p, "user")), "cam": p["cam"], "mic": p["mic"], "screen": p["screen"] })).collect::<Vec<_>>() })).collect();
            let shares: Vec<Value> = hub.shares_for(u).into_iter().map(|x| json!({ "user": x["user"], "name": disp(&s(&x, "user")), "since": x["since"] })).collect();
            let usage = hub.usage_status(a);
            let fmt = |w: &Value| if !w.is_object() { Value::Null } else { json!({ "source": w["source"], "status": w["status"], "usedPercent": w["utilization"].as_f64().map(|x| json!((x * 100.0).round() as i64)).unwrap_or(Value::Null), "resetsInMin": w["resetsAt"].as_f64().map(|r| json!(((r - now as f64) / 60000.0).round().max(0.0) as i64)).unwrap_or(Value::Null) }) };
            let mins = |t: &Value| t.as_str().and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok()).map(|d| json!(((now - d.timestamp_millis()) as f64 / 60000.0).round() as i64)).unwrap_or(Value::Null);
            let hosts: Vec<Value> = hub.ai_hosts_status().into_iter().map(|h| json!({ "nickname": h["nickname"], "owner": if s(&h, "user").is_empty() { String::new() } else { disp(&s(&h, "user")) }, "state": if h["online"] == json!(true) { "up" } else { "down" }, "sinceMin": mins(&h["since"]), "lastSeenMin": mins(&h["seen"]), "version": s(&h, "version"), "offers": h["providers"] })).collect();
            let ppl: Vec<Value> = people().into_iter().map(|mut p| { if let Some(o) = p.as_object_mut() { o.remove("lastSeen"); } p }).collect();
            ok(json!({ "you": u, "people": ppl, "calls": in_calls, "sharedWorkSessions": shares, "aiHosts": hosts, "usage": { "fiveHour": fmt(&usage["five_hour"]), "sevenDay": fmt(&usage["seven_day"]), "context": usage["context"] } }))
        }
        "team_say" => {
            let text = guilds::take_units(s(args, "text").trim(), 20000);
            if text.is_empty() { return no("team_say needs \"text\".".into()); }
            let to = s(args, "to").trim().to_lowercase(); let conv = s(args, "conv").trim().to_string();
            if !to.is_empty() && !conv.is_empty() { return no("Give either \"to\" (a private message) or \"conv\" (a channel / group), not both.".into()); }
            if !to.is_empty() {
                if !us.iter().any(|x| x.0 == to) { return no(format!("No account called \"{to}\".")); }
                if !c.has_key(&to) { return no(format!("{} has no chat key yet (they have not signed in since chat was added), so nothing can be encrypted for them.", disp(&to))); }
            }
            if !conv.is_empty() && conv != "room" {
                let ctx = ctx!(hub, us.clone());
                if !guilds::is_guild_conv(&conv) || !c.guilds.exists(&conv, &ctx) { return no(format!("No conversation \"{conv}\" (use team_channels to list them).")); }
                if !c.guilds.is_member(&conv, u, &ctx) { return no(format!("{} is not in {conv}.", disp(u))); }
            }
            // the person's own rules where they post: timeouts, permissions, the new-member wait, slowmode
            let place = if conv.is_empty() || conv == "room" { "room".to_string() } else { conv.clone() };
            if to.is_empty() { let ctx = ctx!(hub, us.clone()); let why = c.guilds.post_check(&place, u, false, false, &ctx); if !why.is_empty() { return no(why); } }
            let label = format!("{} · Claude", disp(u));
            let seq = agent_say(hub, &text, &to, &label, u, if conv != "room" { &conv } else { "" });
            if to.is_empty() { c.guilds.note_post_by(&place, u); }
            ok(json!({ "posted": true, "seq": seq, "as": label, "to": if !to.is_empty() { to } else if !conv.is_empty() && conv != "room" { conv } else { "everyone".into() } }))
        }
        "team_channels" => {
            let ctx = ctx!(hub, us.clone()); let v = c.guilds.view(u, &ctx);
            ok(json!({ "servers": v["servers"].as_array().cloned().unwrap_or_default().iter().map(|sv| json!({ "id": sv["id"], "name": sv["name"], "members": sv["members"], "channels": sv["channels"].as_array().cloned().unwrap_or_default().iter().map(|ch| json!({ "name": format!("#{}", s(ch, "name")), "conv": ch["conv"], "topic": ch["topic"] })).collect::<Vec<_>>() })).collect::<Vec<_>>(),
                "groups": v["groups"].as_array().cloned().unwrap_or_default().iter().map(|g| json!({ "name": g["name"], "conv": g["conv"], "members": g["members"] })).collect::<Vec<_>>() }))
        }
        "team_presence" => {
            let mut list = people();
            for (x, _, _) in &us { if !list.iter().any(|p| p["user"] == json!(x)) { let st = status_of(hub, x, u, now as u64); list.push(json!({ "user": x, "name": disp(x), "state": "offline", "label": st["label"], "working": st["working"], "acceptingWork": st["acceptingWork"], "dnd": st["dnd"], "text": st["text"], "screen": "", "lastSeen": Value::Null, "agoMin": Value::Null })); } }
            ok(json!({ "you": u, "people": list }))
        }
        _ => no(format!("Unknown team tool: {name}")),
    }
}
fn guilds_iso(ms: i64) -> String { pgm_core_internal::time::iso_at(ms) }
