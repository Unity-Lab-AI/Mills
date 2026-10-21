//! The chat (moved from bridge/server.js, bridge/presence.js, bridge/calls.js, bridge/team.js): end-to-end encrypted
//! messages (this hub keeps public keys, password-wrapped key backups it cannot open, and ciphertext; each reader gets
//! only their own key slot), receipts, typing, presence, the room's long poll, calls (set-up relay + who is in which
//! call), pins / servers / channels / groups (guilds.rs), the team tools of Work runs, and Unity's side of the chat: her
//! key (Chat/agent-key.json, the only key this hub holds), what she is sent, and what she posts. Her brain stays in the
//! JavaScript hub: it hears when she is called (/internal/front/agent-call), asks here for what she was sent, and posts
//! through here. Files: Chat/messages.jsonl, keys.json, seen.json, guilds.json, agent-key.json (the same files);
//! agent-seen.json (what she has read; taken over from agent-state.json).
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use super::auth::Account;
use super::guilds::{self, Ctx, Guilds};
use super::{answer, Routed};
use crate::front::{Hub, Req};
use vr_core::crypto::chat::{ChatKey, Incoming, PubJwk, Wrapped};
use vr_core::now_ms;

pub const AGENT: &str = "claude";
const MAX_CT: usize = 1_600_000;
const TYPING_MS: u64 = 6000; const GONE_MS: u64 = 8000; const ONLINE_MS: u64 = 70000;
const ICE_DEFAULT: &str = r#"[{"urls":"stun:stun.l.google.com:19302"}]"#;

#[derive(Default, Clone)]
struct Pres { online: bool, idle: bool, screen: String, at: u64, seen: u64, polls: u32, gen: u64 }
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
    typing: Mutex<HashMap<String, (String, u64)>>,
    agent_key: ChatKey,
    agent_pub: Value,
    agent_seen: Mutex<i64>,
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
fn read_json(p: &Path) -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) }
fn write_json(p: &Path, v: &Value) { let tmp = p.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp, p); } }
fn s(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_string() }
fn b64ok(v: &Value, max: usize) -> bool { v.as_str().map(|x| x.len() <= max && x.bytes().all(|c| c.is_ascii_alphanumeric() || b"+/=_-".contains(&c))).unwrap_or(false) }
fn dm_of(conv: &str) -> Option<(String, String)> { match guilds::parse(conv) { Some(guilds::Conv::Dm(a, b)) => Some((a, b)), _ => None } }
fn iso() -> String { super::auth::iso_now() }

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
    pub fn new(mills: &Path, root: &Path, cfg: &super::Config, users: &[(String, String, bool)]) -> Chat {
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
        let agent_seen = read_json(&dir.join("agent-seen.json")).and_then(|v| v["lastSeq"].as_i64()).or_else(|| read_json(&dir.join("agent-state.json")).and_then(|v| v["lastSeq"].as_i64())).unwrap_or(0);
        let agent_name = cfg.get("roomAgentName").and_then(Value::as_str).unwrap_or("Unity").to_string();
        let handle: String = agent_name.to_lowercase().split_whitespace().collect();
        let agent_on = !users.iter().any(|u| u.0 == AGENT) && cfg.get("roomAgent") != Some(&json!(false));
        let g = Guilds::new(&dir, root.join("server-owners.json"), &agent_name, users);
        Chat { dir, store: Mutex::new(Store { messages, seq, rev }), keys: Mutex::new(keys), seen: Mutex::new(seen), seen_dirty: Notify::new(), guilds: g, room_ver: AtomicU64::new(1), keys_ver: AtomicU64::new(1), room_woke: Notify::new(),
            presence: Mutex::default(), typing: Mutex::default(), agent_key, agent_pub, agent_seen: Mutex::new(agent_seen), agent_busy: Mutex::new(None), calls: Mutex::default(), call_woke: Mutex::default(), avatars: Mutex::default(), agent_name, agent_on, handle, mills: mills.to_path_buf() }
    }
    pub fn room_changed(&self) { self.room_ver.fetch_add(1, Ordering::SeqCst); self.room_woke.notify_waiters(); }
    pub fn keys_changed(&self) { self.keys_ver.fetch_add(1, Ordering::SeqCst); self.avatars.lock().unwrap().clear(); self.room_changed(); }
    pub fn has_key(&self, u: &str) -> bool { self.keys.lock().unwrap().contains_key(u) }
    fn pub_of(&self, u: &str) -> Option<Value> { self.keys.lock().unwrap().get(u).map(|k| k["pub"].clone()) }
    fn save_keys(&self) { write_json(&self.dir.join("keys.json"), &Value::Object(self.keys.lock().unwrap().clone())); }
    pub fn drop_key(&self, u: &str) { if self.keys.lock().unwrap().remove(u).is_some() { self.save_keys(); } }
    fn agent_enabled(&self, users: &[(String, String, bool)]) -> bool { self.agent_on && !users.iter().any(|u| u.0 == AGENT) }
    pub fn agent_on(&self, users: &[(String, String, bool)]) -> bool { self.agent_enabled(users) }
    /// Unity composing (target, conv) or not: everyone's "is typing".
    pub fn set_agent_busy(&self, v: Option<(String, String)>) { *self.agent_busy.lock().unwrap() = v; self.room_changed(); }
    pub fn seq(&self) -> i64 { self.store.lock().unwrap().seq }
    /// The conversation of a message (None when deleted or unknown): pins and the guilds ask.
    pub fn msg_conv(&self, seq: i64) -> Option<String> { self.store.lock().unwrap().messages.iter().find(|m| m["seq"].as_i64() == Some(seq) && m["deleted"] != json!(true)).map(conv_of) }
    fn save_messages(store: &Store, dir: &Path) {
        let f = dir.join("messages.jsonl"); let tmp = f.with_extension("jsonl.tmp");
        let mut out = String::new(); for m in &store.messages { out.push_str(&m.to_string()); out.push('\n'); }
        if std::fs::write(&tmp, out).is_ok() { let _ = std::fs::rename(&tmp, &f); }
    }
    fn append(&self, mut msg: Value) -> i64 {
        let seqn = msg["seq"].as_i64().unwrap_or(0);
        { let mut st = self.store.lock().unwrap(); st.rev += 1; msg["rev"] = json!(st.rev);
          use std::io::Write; if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("messages.jsonl")) { let _ = writeln!(f, "{msg}"); }
          st.messages.push(msg); }
        self.room_changed(); seqn
    }
    /// Deletes a person's messages in these conversations since then (a ban that deletes): their readers lose the files.
    fn delete_recent(&self, user: &str, convs: &[String], since: i64, files: &super::files::Files, acct: &dyn Fn(&str) -> Option<Account>) -> usize {
        let mut gone_files: Vec<String> = vec![]; let mut n = 0;
        { let mut st = self.store.lock().unwrap(); let mut rev = st.rev;
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
fn users(hub: &Hub) -> Vec<(String, String, bool)> { hub.mods.auth.accounts().into_iter().map(|a| (a.username, a.display, a.admin)).collect() }
/// One call's Ctx for the guilds (the accounts now, the chat's seq, pins' conversations, a ban's deletes).
macro_rules! ctx { ($hub:expr, $users:expr) => { Ctx { users: &$users, seq: $hub.mods.chat.seq(), msg_conv: &|n| $hub.mods.chat.msg_conv(n), delete_recent: &|u: &str, c: &[String], since: i64| $hub.mods.chat.delete_recent(u, c, since, &$hub.mods.files, &|x| $hub.mods.auth.account(x)) } } }
fn reader(hub: &Hub, user: &str, us: &[(String, String, bool)]) -> Reader {
    let c = &hub.mods.chat; let ctx = ctx!(hub, us.to_vec());
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
pub fn may_read(hub: &Hub, user: &str, seq: i64) -> bool {
    let us = users(hub); let rd = reader(hub, user, &us);
    let st = hub.mods.chat.store.lock().unwrap();
    st.messages.iter().find(|m| m["seq"].as_i64() == Some(seq)).map(|m| m["deleted"] != json!(true) && rd.visible(m) && !m["keys"][user].is_null()).unwrap_or(false)
}
fn conv_member(hub: &Hub, conv: &str, user: &str, us: &[(String, String, bool)]) -> bool {
    if conv == "room" { return true; }
    if let Some(r) = conv.strip_prefix("dm:") { return r.split('|').any(|x| x == user); }
    let ctx = ctx!(hub, us.to_vec()); hub.mods.chat.guilds.is_member(conv, user, &ctx)
}

// ---------- presence + typing ----------
impl Chat {
    fn pres_set(&self, user: &str, f: impl FnOnce(&mut Pres)) {
        let changed = { let mut m = self.presence.lock().unwrap(); let p = m.entry(user.to_string()).or_default(); let before = (p.online, p.idle, p.screen.clone()); f(p); p.seen = now_ms(); let after = (p.online, p.idle, p.screen.clone()); if before != after { p.at = now_ms(); true } else { false } };
        if changed { self.room_changed(); }
    }
    pub fn touch(&self, user: &str) { self.presence.lock().unwrap().entry(user.to_string()).or_default().seen = now_ms(); }
    fn poll_opened(&self, user: &str) { { let mut m = self.presence.lock().unwrap(); let p = m.entry(user.to_string()).or_default(); p.polls += 1; p.gen += 1; } self.pres_set(user, |p| p.online = true); }
    fn poll_closed(self: &Arc<Self>, user: &str) {
        let gen = { let mut m = self.presence.lock().unwrap(); let p = m.entry(user.to_string()).or_default(); p.polls = p.polls.saturating_sub(1); if p.polls > 0 { return; } p.gen += 1; p.gen };
        let (c, u) = (self.clone(), user.to_string());
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(GONE_MS)).await; let still = c.presence.lock().unwrap().get(&u).map(|p| p.gen == gen && p.polls == 0).unwrap_or(false); if still { c.pres_set(&u, |p| { p.online = false; p.idle = false; }); } });
    }
    pub fn gone(&self, user: &str) { { let mut m = self.presence.lock().unwrap(); let p = m.entry(user.to_string()).or_default(); p.polls = 0; p.gen += 1; } self.pres_set(user, |p| { p.online = false; p.idle = false; }); }
    pub fn presence_list(&self, us: &[(String, String, bool)]) -> Vec<Value> {
        let now = now_ms(); let mut m = self.presence.lock().unwrap();
        let mut out: Vec<(String, Value)> = vec![];
        for (u, p) in m.iter_mut() {
            if !us.iter().any(|x| x.0 == *u) { continue; }
            if p.online && now - p.seen > ONLINE_MS { p.online = false; p.idle = false; p.at = now; }
            out.push((u.clone(), json!({ "user": u, "online": p.online, "idle": p.online && p.idle, "screen": if p.online { p.screen.clone() } else { String::new() }, "at": p.at, "seen": p.seen })));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0)); out.into_iter().map(|x| x.1).collect()
    }
    pub fn online(&self, us: &[(String, String, bool)]) -> Vec<String> { self.presence_list(us).into_iter().filter(|p| p["online"] == json!(true)).map(|p| s(&p, "user")).collect() }
    pub fn set_typing(self: &Arc<Self>, user: &str, conv: &str, on: bool) {
        let prev = self.typing.lock().unwrap().remove(user);
        if !on { if prev.is_some() { self.room_changed(); } return; }
        let at = now_ms(); self.typing.lock().unwrap().insert(user.to_string(), (conv.to_string(), at));
        let (c, u) = (self.clone(), user.to_string());
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(TYPING_MS)).await; let lapsed = { let mut t = c.typing.lock().unwrap(); if t.get(&u).map(|x| x.1 == at).unwrap_or(false) { t.remove(&u); true } else { false } }; if lapsed { c.room_changed(); } });
        if prev.map(|p| p.0 != conv).unwrap_or(true) { self.room_changed(); }
    }
}
fn typing_for(hub: &Hub, user: &str, us: &[(String, String, bool)]) -> Vec<Value> {
    let c = &hub.mods.chat;
    let list: Vec<(String, String)> = c.typing.lock().unwrap().iter().map(|(u, t)| (u.clone(), t.0.clone())).collect();
    let mut out: Vec<Value> = list.into_iter().filter(|(u, conv)| u != user && conv_member(hub, conv, user, us)).map(|(u, conv)| json!({ "user": u, "conv": conv })).collect();
    if let Some((target, conv)) = c.agent_busy.lock().unwrap().clone() { if c.agent_enabled(us) && (target.is_empty() || target == user) { out.push(json!({ "user": AGENT, "conv": if target.is_empty() { if conv.is_empty() { "room".to_string() } else { conv } } else { let mut v = [AGENT.to_string(), user.to_string()]; v.sort(); format!("dm:{}|{}", v[0], v[1]) } })); } }
    out
}

// ---------- Unity: what she reads, what she posts ----------
impl Chat {
    fn agent_open(&self, m: &Value) -> Option<String> {
        let slot = &m["keys"][AGENT];
        if !slot.is_object() || m["deleted"] == json!(true) { return None; }
        let inc = Incoming { v: m["v"].as_u64().map(|v| v as u8), mid: m["mid"].as_str().map(String::from), from: s(m, "from"), to: m["to"].as_str().map(String::from), iv: s(m, "iv"), ct: s(m, "ct"), key: Wrapped { iv: s(slot, "iv"), wk: s(slot, "wk") }, from_pub: serde_json::from_value(m["fromPub"].clone()).ok()? };
        vr_core::crypto::chat::open(&self.agent_key, AGENT, &inc).ok()
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
pub fn agent_say(hub: &Hub, text: &str, to: &str, as_: &str, via: &str, conv: &str) -> i64 {
    let c = &hub.mods.chat; let us = users(hub);
    let conv = if to.is_empty() && guilds::is_guild_conv(conv) { conv.to_string() } else { String::new() };
    let allowed = if conv.is_empty() { None } else { let ctx = ctx!(hub, us.clone()); Some(c.guilds.members(&conv, &ctx).unwrap_or_default()) };
    let mid = vr_core::crypto::chat::new_mid();
    let aad = vr_core::crypto::chat::body_aad(AGENT, to, &mid, 3);
    let raw: [u8; 32] = vr_core::crypto::random_bytes(32).try_into().expect("32");
    let iv = vr_core::crypto::random_bytes(12);
    let Ok(ct) = vr_core::crypto::aes_gcm_seal(&raw, &iv, aad.as_bytes(), &vr_core::crypto::chat::pad_plain(text)) else { return 0 };
    let mut readers: Vec<(String, Value)> = vec![(AGENT.into(), c.agent_pub.clone())];
    { let keys = c.keys.lock().unwrap(); for (u, _, _) in &us { if let Some(k) = keys.get(u) { if (to.is_empty() || u == to) && allowed.as_ref().map(|l| l.contains(u)).unwrap_or(true) { readers.push((u.clone(), k["pub"].clone())); } } } }
    let mut slots = Map::new();
    for (u, p) in readers {
        let Ok(pj) = serde_json::from_value::<PubJwk>(p) else { continue };
        let Ok(wk) = c.agent_key.wrap_key(&pj) else { continue };
        let kiv = vr_core::crypto::random_bytes(12);
        if let Ok(w) = vr_core::crypto::aes_gcm_seal(&wk, &kiv, format!("{aad}|{u}").as_bytes(), &raw) { slots.insert(u, json!({ "iv": vr_core::b64(&kiv), "wk": vr_core::b64(&w) })); }
    }
    let seq = { let mut st = c.store.lock().unwrap(); st.seq += 1; st.seq };
    let mut msg = json!({ "seq": seq, "v": 3, "mid": mid, "from": AGENT });
    if !to.is_empty() { msg["to"] = json!(to); }
    if !conv.is_empty() { msg["conv"] = json!(conv); }
    for (k, v) in [("ts", json!(iso())), ("fromPub", c.agent_pub.clone()), ("iv", json!(vr_core::b64(&iv))), ("ct", json!(vr_core::b64(&ct))), ("keys", Value::Object(slots))] { msg[k] = v; }
    if !as_.is_empty() { msg["as"] = json!(guilds::take_units(as_, 60)); }
    if !via.is_empty() && us.iter().any(|u| u.0 == via) { msg["via"] = json!(via); }
    c.append(msg)
}
/// A message just in: is Unity called? (a private message to her, or @<her name> where she may read it)
async fn agent_hear(hub: &Arc<Hub>, msg: &Value) {
    let c = &hub.mods.chat; let us = users(hub);
    if !c.agent_enabled(&us) || msg["from"] == json!(AGENT) { return; }
    let off = hub.root.join("ai-host.off").exists();
    let off_note = |to: &str, conv: &str| { agent_say(hub, "I can't answer right now: my host paused the AI. Ask me again once it is back.", to, "", "", conv); };
    let from = s(msg, "from");
    let call = |target: String, caller: String, conv: String| super::unity_routes::run_agent(hub, target, caller, conv);
    if msg["to"] == json!(AGENT) { if c.agent_read(msg).is_some() { if off { return off_note(&from, ""); } call(from.clone(), from, String::new()); } return; }
    if !s(msg, "to").is_empty() { return; }
    let Some(text) = c.agent_read(msg) else { return };
    let low = text.to_lowercase(); let h = format!("@{}", c.handle);
    let called = low.match_indices(&h).any(|(i, _)| { let after = low[i + h.len()..].chars().next(); !after.map(|ch| ch.is_alphanumeric() || ch == '_').unwrap_or(false) });
    if called { if off { return off_note("", msg["conv"].as_str().unwrap_or("")); } call(String::new(), from, s(msg, "conv")); }
}
/// What Unity was sent since she last spoke (with the few messages before each call): her brain's prompt.
pub fn agent_blocks(hub: &Hub) -> String {
    let c = &hub.mods.chat;
    let since = *c.agent_seen.lock().unwrap();
    let (list, seq): (Vec<Value>, i64) = { let st = c.store.lock().unwrap(); let l: Vec<Value> = st.messages.iter().filter(|m| m["seq"].as_i64().unwrap_or(0) > since && m["from"] != json!(AGENT)).cloned().collect(); (l, st.seq) };
    let blocks: Vec<String> = list.iter().skip(list.len().saturating_sub(6)).filter_map(|m| {
        let t = c.agent_read(m)?;
        let ctx: Vec<String> = c.agent_context(m).into_iter().map(|(n, t)| format!("  {n}: {t}")).collect();
        Some(format!("{}[{}] {}{}: {t}", if ctx.is_empty() { String::new() } else { format!("Earlier in that conversation:\n{}\n", ctx.join("\n")) }, s(m, "ts"), if m["to"] == json!(AGENT) { "(private message to you) " } else { "" }, hub.mods.name_of(&s(m, "from"))))
    }).collect();
    *c.agent_seen.lock().unwrap() = seq; write_json(&c.dir.join("agent-seen.json"), &json!({ "lastSeq": seq }));
    if blocks.is_empty() { "(none)".into() } else { blocks.join("\n\n") }
}

// ---------- members + profiles (kept by the JavaScript hub in state.json; pictures in Avatars/) ----------
fn avatar(hub: &Hub, u: &str) -> String {
    let c = &hub.mods.chat;
    if let Some(a) = c.avatars.lock().unwrap().get(u) { return a.clone(); }
    let mut url = String::new();
    for (ext, mime) in [("webp", "image/webp"), ("png", "image/png"), ("jpg", "image/jpeg")] { if let Ok(b) = std::fs::read(c.mills.join("Avatars").join(format!("{u}.{ext}"))) { url = format!("data:{mime};base64,{}", vr_core::b64(&b)); break; } }
    c.avatars.lock().unwrap().insert(u.to_string(), url.clone()); url
}
pub fn public_prefs_of(hub: &Hub, a: &Account) -> Value { public_prefs(hub, a) }
fn public_prefs(hub: &Hub, a: &Account) -> Value {
    let st = read_json(&a.dir.join("state.json")).unwrap_or(json!({}));
    let p = &st["profile"];
    json!({ "nick": st["prefs"]["nick"].as_str().unwrap_or(""), "color": if st["prefs"]["color"].is_null() { Value::Null } else { st["prefs"]["color"].clone() },
        "profile": { "bio": s(p, "bio"), "pronouns": s(p, "pronouns"), "status": s(p, "status"), "location": s(p, "location"), "links": if p["links"].is_array() { p["links"].clone() } else { json!([]) }, "accent": if p["accent"].is_null() { Value::Null } else { p["accent"].clone() }, "timezone": s(p, "timezone"), "avatar": avatar(hub, &a.username) } })
}

// ---------- calls ----------
impl Chat {
    fn call_wake(&self, users: &[String]) { let w = self.call_woke.lock().unwrap(); for u in users { if let Some(n) = w.get(u) { n.notify_waiters(); } } }
    fn call_notify(&self, u: &str) -> Arc<Notify> { self.call_woke.lock().unwrap().entry(u.to_string()).or_default().clone() }
}
fn calls_changed(hub: &Hub, conv: &str, us: &[(String, String, bool)]) { let c = &hub.mods.chat; c.calls.lock().unwrap().ver += 1; let who: Vec<String> = us.iter().map(|x| x.0.clone()).filter(|u| conv_member(hub, conv, u, us)).collect(); c.call_wake(&who); }
pub fn calls_state(hub: &Hub, user: &str, us: &[(String, String, bool)]) -> Value {
    let c = &hub.mods.chat; let list = c.calls.lock().unwrap().calls.clone();
    let ctx = ctx!(hub, us.to_vec());
    let mut out = Map::new();
    for (conv, ps) in list { if ps.is_empty() || !conv_member(hub, &conv, user, us) { continue; }
        out.insert(conv.clone(), json!(ps.iter().map(|(u, p)| { let md = c.guilds.media(&conv, u, &ctx); let held = md["speak"] != json!(true) || md["muted"] == json!(true); let stream = md["stream"] == json!(true);
            let mut v = json!({ "user": u, "cam": p["cam"] == json!(true) && stream, "mic": p["mic"] == json!(true) && !held, "screen": p["screen"] == json!(true) && stream, "since": p["since"] });
            if held { v["held"] = json!(if md["muted"] == json!(true) { "muted" } else { "nospeak" }); } if !stream { v["nostream"] = json!(true); } v }).collect::<Vec<_>>())); }
    Value::Object(out)
}
pub fn calls_leave(hub: &Hub, user: &str) {
    let us = users(hub); let mut changed = vec![];
    { let mut cs = hub.mods.chat.calls.lock().unwrap(); for (c, ps) in cs.calls.iter_mut() { let n = ps.len(); ps.retain(|(u, _)| u != user); if ps.len() != n { changed.push(c.clone()); } } cs.calls.retain(|(_, ps)| !ps.is_empty()); cs.mailbox.remove(user); }
    for c in changed { calls_changed(hub, &c, &us); }
}
pub fn start(hub: &Arc<Hub>) {
    // browsers long-poll at least every 25 s while in a call; 60 s of silence = gone
    let h = hub.clone();
    tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(10)).await; let now = now_ms(); let mut changed = vec![];
        { let mut cs = h.mods.chat.calls.lock().unwrap(); for (c, ps) in cs.calls.iter_mut() { let n = ps.len(); ps.retain(|(_, p)| now.saturating_sub(p["seen"].as_u64().unwrap_or(0)) <= 60000); if ps.len() != n { changed.push(c.clone()); } } cs.calls.retain(|(_, ps)| !ps.is_empty()); }
        if !changed.is_empty() { let us = users(&h); for c in changed { calls_changed(&h, &c, &us); } } } });
    // temporary members who went offline leave; ended timeouts go
    let h = hub.clone();
    tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(20)).await; let us = users(&h); let on = h.mods.chat.online(&us); if h.mods.chat.guilds.sweep(&|u| on.iter().any(|x| x == u)) { h.mods.chat.room_changed(); } } });
    // receipts are saved half a second after a change
    let h = hub.clone();
    tokio::spawn(async move { loop { h.mods.chat.seen_dirty.notified().await; tokio::time::sleep(Duration::from_millis(500)).await; let v = Value::Object(h.mods.chat.seen.lock().unwrap().clone()); write_json(&h.mods.chat.dir.join("seen.json"), &v); } });
    // the auto-deploy went back to the last good version: say so once
    let h = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let f = h.root.join(".rolled-back"); if !f.exists() { return; }
        let text = std::fs::read_to_string(&f).unwrap_or_default().trim().to_string(); let _ = std::fs::remove_file(&f);
        agent_say(&h, &format!("**VibeRoller rolled itself back.** {} Nothing was lost: deploys never touch data. Whoever shipped it: look at the deploy log, fix, and ship again.", if text.is_empty() { "A new version did not come up, so the hub went back to the last good one." } else { &text }), "", "VibeRoller", "", "");
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
pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    let c = &hub.mods.chat; let u = a.username.as_str();
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let p = r.path.as_str();
    if !(p.starts_with("/api/room/") || p == "/api/presence" || p.starts_with("/api/call/") || p == "/api/guilds" || p.starts_with("/api/guilds/") || p == "/api/admin/room/clear") { return None; }
    let us = users(hub); let agent_on = c.agent_enabled(&us);
    let b = r.json();
    Some(match p {
        "/api/room/members" if get => {
            let keys = c.keys.lock().unwrap().clone();
            let mut list: Vec<Value> = hub.mods.auth.accounts().iter().map(|acc| { let pp = public_prefs(hub, acc); json!({ "username": acc.username, "display": acc.display, "nick": pp["nick"], "color": pp["color"], "profile": pp["profile"], "pub": keys.get(&acc.username).map(|k| k["pub"].clone()).unwrap_or(Value::Null) }) }).collect();
            if agent_on { list.push(json!({ "username": AGENT, "display": c.agent_name, "nick": "", "color": Value::Null, "pub": c.agent_pub, "agent": true })); }
            answer(200, json!({ "members": list }))
        }
        "/api/room/mykey" if get => answer(200, c.keys.lock().unwrap().get(u).cloned().unwrap_or(json!({}))),
        "/api/room/mykey" if post => {
            let (pubk, backup) = (&b["pub"], &b["backup"]);
            if !valid_jwk(pubk) || !backup.is_object() || !b64ok(&backup["salt"], 64) || !b64ok(&backup["iv"], 64) || !b64ok(&backup["ct"], 4096) { return Some(answer(400, json!({ "error": "Bad key." }))); }
            c.keys.lock().unwrap().insert(u.into(), json!({ "pub": { "kty": "EC", "crv": "P-256", "x": pubk["x"], "y": pubk["y"] }, "backup": { "salt": backup["salt"], "iv": backup["iv"], "ct": backup["ct"] }, "updated": iso() }));
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
            if !v.is_null() && !(matches!(v.as_u64(), Some(3 | 4)) && mid_ok(&b["mid"]) && !c.store.lock().unwrap().messages.iter().any(|x| x["mid"] == b["mid"])) { return Some(answer(400, json!({ "error": "Bad message." }))); }
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
            let seqn = { let mut st = c.store.lock().unwrap(); st.seq += 1; st.seq };
            let file_ids = if b.get("files").is_some() { match hub.mods.files.attach(Some(a), u, &b["files"], seqn, &file_conv) { Ok(ids) => ids, Err(f) => return Some(answer(f.status, json!({ "error": f.msg }))) } } else { vec![] };
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
            let m = c.store.lock().unwrap().messages.iter().find(|x| x["seq"].as_i64() == Some(target)).cloned();
            let Some(m) = m.filter(|m| m["from"] == json!(u) && m["deleted"] != json!(true)) else { return Some(answer(403, json!({ "error": "You can only edit your own messages." }))) };
            let ctx = ctx!(hub, us.clone());
            let why = if s(&m, "to").is_empty() { c.guilds.post_check(m["conv"].as_str().filter(|x| !x.is_empty()).unwrap_or("room"), u, false, true, &ctx) } else { String::new() };
            if !why.is_empty() { return Some(answer(403, json!({ "error": why }))); }
            let Some(mine) = c.pub_of(u).filter(|_| b64ok(&b["iv"], 64) && b64ok(&b["ct"], MAX_CT)) else { return Some(answer(400, json!({ "error": "Bad message." }))) };
            let v = &b["v"];
            if !v.is_null() && !(matches!(v.as_u64(), Some(3 | 4)) && mid_ok(&b["mid"]) && (if m["mid"].is_string() { m["mid"] == b["mid"] } else { !c.store.lock().unwrap().messages.iter().any(|x| x["mid"] == b["mid"]) })) { return Some(answer(400, json!({ "error": "Bad message." }))); }
            if m["blob"].is_string() && v.as_u64() != Some(4) { return Some(answer(400, json!({ "error": "Bad message." }))); }
            let conv = s(&m, "conv"); let allowed = if conv.is_empty() { None } else { c.guilds.members(&conv, &ctx) };
            let slots = clean_slots(&b["keys"], u, &s(&m, "to"), allowed.as_deref(), &us, agent_on);
            if !slots.contains_key(u) { return Some(answer(400, json!({ "error": "Bad message." }))); }
            let rev = { let mut st = c.store.lock().unwrap(); st.rev += 1; let rev = st.rev;
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
            let m = c.store.lock().unwrap().messages.iter().find(|x| x["seq"].as_i64() == Some(target)).cloned();
            let ok = match &m { None => false, Some(m) => m["from"] == json!(u) || (s(m, "to").is_empty() && { let ctx = ctx!(hub, us.clone()); c.guilds.moderates(m["conv"].as_str().filter(|x| !x.is_empty()).unwrap_or("room"), u, &ctx) } && reader(hub, u, &us).visible(m)) };
            if !ok { return Some(answer(403, json!({ "error": "You can only delete your own messages." }))); }
            let mut gone = vec![];
            let rev = { let mut st = c.store.lock().unwrap(); let mut rev = st.rev; let mut changed = false;
                if let Some(x) = st.messages.iter_mut().find(|x| x["seq"].as_i64() == Some(target)) { if x["deleted"] != json!(true) { tombstone(x, &mut gone); rev += 1; x["rev"] = json!(rev); changed = true; } else { rev = x["rev"].as_i64().unwrap_or(rev); } }
                if changed { st.rev = rev; Chat::save_messages(&st, &c.dir); } rev };
            if !gone.is_empty() { hub.mods.files.message_gone(&gone, &|x| hub.mods.auth.account(x)); }
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
                let changed = { let mut seen = c.seen.lock().unwrap(); let mine = seen.entry(u.to_string()).or_insert(json!({})); if v > mine[&conv].as_i64().unwrap_or(0) { mine[&conv] = json!(v); true } else { false } };
                if changed { c.seen_dirty.notify_one(); c.room_changed(); }
            }
            answer(200, json!({ "ok": true }))
        }
        "/api/room/messages" if get => {
            if r.param("wait").as_deref() != Some("1") { c.touch(u); return Some(room_answer(hub, r, u, &us)); }
            // judged before this page's own coming online moves the version; listening starts after it
            let stale = r.param("ver").and_then(|x| x.parse::<u64>().ok()) != Some(c.room_ver.load(Ordering::SeqCst));
            c.poll_opened(u);
            let woke = c.room_woke.notified(); tokio::pin!(woke); woke.as_mut().enable();
            struct Open(Arc<Chat>, String);
            impl Drop for Open { fn drop(&mut self) { self.0.poll_closed(&self.1); } }
            let chat_arc = hub.mods.chat.clone();
            let guard = Open(chat_arc, u.to_string());
            if !stale { let _ = tokio::time::timeout(Duration::from_secs(25), woke).await; }
            drop(guard);
            room_answer(hub, r, u, &us)
        }
        "/api/presence" if post => {
            if !(b["gone"].is_null() || b["gone"] == json!(false)) { c.gone(u); }
            else { let screen = b["screen"].as_str().map(|x| guilds::take_units(x, 12)); let idle = b.get("idle").map(|x| !(x.is_null() || *x == json!(false) || *x == json!(0) || *x == json!(""))); c.pres_set(u, |pr| { if let Some(sc) = screen { pr.screen = sc; } if let Some(i) = idle { pr.idle = i; } }); }
            answer(200, json!({ "ok": true }))
        }
        "/api/room/blob" if get => {
            let n = r.param("seq").and_then(|x| x.parse::<i64>().ok()).unwrap_or(-1);
            let rd = reader(hub, u, &us);
            let st = c.store.lock().unwrap();
            match st.messages.iter().find(|m| m["seq"].as_i64() == Some(n)).filter(|m| m["blob"].is_string() && rd.visible(m) && !m["keys"][u].is_null()) { Some(m) => answer(200, json!({ "blob": m["blob"] })), None => answer(404, json!({ "error": "Not found." })) }
        }
        p if p.starts_with("/api/call/") => return Some(calls_route(hub, r, a, &us, &b).await),
        "/api/guilds" if get => { let ctx = ctx!(hub, us.clone()); answer(200, c.guilds.view(u, &ctx)) }
        p if post && p.starts_with("/api/guilds/") => match guild_op_name(p) {
            Some(op) => {
                let ctx = ctx!(hub, us.clone());
                let (res, ann) = c.guilds.op(op, u, &if b.is_object() { b.clone() } else { json!({}) }, &ctx);
                for (conv, text) in ann { agent_say(hub, &text, "", "VibeRoller", "", &conv); }
                match res {
                    Ok(v) => { c.room_changed(); let view = c.guilds.view(u, &ctx); let mut out = json!({ "ok": true, "result": if v.is_object() && v.get("password").is_some() && v.get("members").is_some() { json!({ "id": v["id"], "name": v["name"] }) } else { v } }); for (k, x) in view.as_object().cloned().unwrap_or_default() { out[k] = x; } answer(200, out) }
                    Err(e) => answer(400, json!({ "error": e })),
                }
            }
            None => answer(404, json!({ "error": "Unknown endpoint" })),
        },
        "/api/admin/room/clear" if post => {
            if !a.admin { return Some(answer(403, json!({ "error": "Admins only." }))); }
            { let mut st = c.store.lock().unwrap(); st.messages.clear(); st.rev = 0; let _ = std::fs::write(c.dir.join("messages.jsonl"), ""); }
            hub.mods.files.all_gone(&|x| hub.mods.auth.account(x));
            *c.agent_seen.lock().unwrap() = 0; write_json(&c.dir.join("agent-seen.json"), &json!({ "lastSeq": 0 }));
            c.seen.lock().unwrap().clear(); write_json(&c.dir.join("seen.json"), &json!({}));
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
fn room_answer(hub: &Hub, r: &Req, u: &str, us: &[(String, String, bool)]) -> Routed {
    let c = &hub.mods.chat;
    let rd = reader(hub, u, us);
    let strip = |m: &Value| -> Value { let mut o = m.as_object().cloned().unwrap_or_default(); let key = o.remove("keys").map(|k| k[u].clone()).filter(|k| !k.is_null()).unwrap_or(Value::Null); let blob = o.remove("blob"); o.insert("key".into(), key); if let Some(bl) = blob.and_then(|b| b.as_str().map(|x| x.len())) { o.insert("blobLen".into(), json!(bl)); } Value::Object(o) };
    let (out, latest, latest_rev): (Vec<Value>, i64, i64) = {
        let st = c.store.lock().unwrap();
        let mine: Vec<&Value> = st.messages.iter().filter(|m| rd.visible(m)).collect();
        let pick: Vec<&Value> = if let Some(rv) = r.param("rev") {
            let after: i64 = rv.parse::<f64>().map(|f| f as i64).unwrap_or(0);
            if after != 0 { mine.into_iter().filter(|m| m["rev"].as_i64().or(m["seq"].as_i64()).unwrap_or(0) > after).collect() } else { let k = mine.len().saturating_sub(500); mine[k..].to_vec() }
        } else { let since: i64 = r.param("since").and_then(|x| x.parse::<f64>().ok()).map(|f| f as i64).unwrap_or(0); let l: Vec<&Value> = mine.into_iter().filter(|m| m["seq"].as_i64().unwrap_or(0) > since).collect(); let k = l.len().saturating_sub(300); l[k..].to_vec() };
        (pick.into_iter().map(strip).collect(), st.seq, st.rev)
    };
    let mut seen_out = Map::new();
    for (who, convs) in c.seen.lock().unwrap().iter() { for (cv, v) in convs.as_object().cloned().unwrap_or_default() { if rd.sees_conv(&cv) { seen_out.entry(who.clone()).or_insert(json!({}))[&cv] = v; } } }
    answer(200, json!({ "messages": out, "latest": latest, "latestRev": latest_rev, "seen": seen_out, "shares": hub.mods.work.shares_for(u), "ver": c.room_ver.load(Ordering::SeqCst), "keysVer": c.keys_ver.load(Ordering::SeqCst), "guildsVer": c.guilds.version(), "notesVer": hub.mods.notes.version(),
        "online": c.online(us), "presence": c.presence_list(us), "typing": typing_for(hub, u, us) }))
}
async fn calls_route(hub: &Arc<Hub>, r: &Req, a: &Account, us: &[(String, String, bool)], b: &Value) -> Routed {
    let c = &hub.mods.chat; let u = a.username.as_str();
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
            { let mut cs = c.calls.lock().unwrap();
              for (cv, ps) in cs.calls.iter_mut() { if *cv != conv { let n = ps.len(); ps.retain(|(x, _)| x != u); if ps.len() != n { left.push(cv.clone()); } } }
              cs.calls.retain(|(_, ps)| !ps.is_empty());
              if !cs.calls.iter().any(|(cv, _)| *cv == conv) { cs.calls.push((conv.clone(), vec![])); }
              let ps = &mut cs.calls.iter_mut().find(|(cv, _)| *cv == conv).unwrap().1;
              let since = ps.iter().find(|(x, _)| x == u).map(|(_, p)| p["since"].clone()).unwrap_or(json!(iso()));
              ps.retain(|(x, _)| x != u); ps.push((u.to_string(), json!({ "cam": flag("cam"), "mic": flag("mic"), "screen": flag("screen"), "since": since, "seen": now_ms() }))); }
            for cv in left { calls_changed(hub, &cv, us); }
            calls_changed(hub, &conv, us);
            let ice = hub.cfg.get("iceServers").filter(|v| v.is_array()).cloned().unwrap_or_else(|| serde_json::from_str(ICE_DEFAULT).unwrap());
            answer(200, json!({ "calls": calls_state(hub, u, us), "ver": c.calls.lock().unwrap().ver, "iceServers": ice }))
        }
        ("POST", "/api/call/leave") => { calls_leave(hub, u); answer(200, json!({ "ok": true })) }
        ("POST", "/api/call/state") => {
            let conv = s(b, "conv");
            let found = { let mut cs = c.calls.lock().unwrap(); match cs.calls.iter_mut().find(|(cv, _)| *cv == conv).and_then(|(_, ps)| ps.iter_mut().find(|(x, _)| x == u)) { Some((_, pv)) => { pv["cam"] = json!(flag("cam")); pv["mic"] = json!(flag("mic")); pv["screen"] = json!(flag("screen")); pv["seen"] = json!(now_ms()); true } None => false } };
            if !found { return answer(409, json!({ "error": "You are not in that call." })); }
            calls_changed(hub, &conv, us); answer(200, json!({ "ok": true }))
        }
        ("POST", "/api/call/signal") => {
            let (to, conv) = (s(b, "to"), s(b, "conv"));
            let both = { let cs = c.calls.lock().unwrap(); cs.calls.iter().find(|(cv, _)| *cv == conv).map(|(_, ps)| ps.iter().any(|(x, _)| x == u) && ps.iter().any(|(x, _)| *x == to)).unwrap_or(false) };
            if !both || to == u { return answer(409, json!({ "error": "Both people must be in this call." })); }
            if !b64ok(&b["iv"], 64) || !b64ok(&b["ct"], 200000) { return answer(400, json!({ "error": "Bad signal." })); }
            { let mut cs = c.calls.lock().unwrap(); cs.n += 1; let n = cs.n; let bx = cs.mailbox.entry(to.clone()).or_default(); bx.push(json!({ "n": n, "from": u, "conv": conv, "iv": b["iv"], "ct": b["ct"] })); if bx.len() > 500 { let k = bx.len() - 500; bx.drain(0..k); } }
            c.call_wake(&[to]); answer(200, json!({ "ok": true }))
        }
        ("GET", "/api/call/poll") => {
            let after: u64 = r.param("after").and_then(|x| x.parse::<f64>().ok()).map(|f| f as u64).unwrap_or(0); let ver: Option<u64> = r.param("ver").and_then(|x| x.parse().ok());
            { let mut cs = c.calls.lock().unwrap(); if let Some(bx) = cs.mailbox.get_mut(u) { bx.retain(|x| x["n"].as_u64().unwrap_or(0) > after); } }
            let woke = c.call_notify(u); let n = woke.notified(); tokio::pin!(n); n.as_mut().enable();
            let ready = { let cs = c.calls.lock().unwrap(); cs.mailbox.get(u).map(|bx| bx.iter().any(|x| x["n"].as_u64().unwrap_or(0) > after)).unwrap_or(false) || ver != Some(cs.ver) };
            if !ready { let _ = tokio::time::timeout(Duration::from_secs(25), n).await; }
            let (sigs, v) = { let mut cs = c.calls.lock().unwrap(); for (_, ps) in cs.calls.iter_mut() { if let Some((_, pv)) = ps.iter_mut().find(|(x, _)| x == u) { pv["seen"] = json!(now_ms()); } } (cs.mailbox.get(u).map(|bx| bx.iter().filter(|x| x["n"].as_u64().unwrap_or(0) > after).cloned().collect::<Vec<_>>()).unwrap_or_default(), cs.ver) };
            let last = sigs.last().and_then(|x| x["n"].as_u64()).unwrap_or(after);
            answer(200, json!({ "signals": sigs, "after": last, "calls": calls_state(hub, u, us), "ver": v }))
        }
        _ => answer(404, json!({ "error": "Unknown endpoint" })),
    }
}

/// The JavaScript hub asking (its Unity: post, read what she was sent, show her composing; its profiles changed).
pub fn front(hub: &Arc<Hub>, what: &str, b: &Value) -> Value {
    let c = &hub.mods.chat;
    match what {
        "say" => json!({ "seq": agent_say(hub, &s(b, "text"), &s(b, "to"), &s(b, "as"), &s(b, "via"), &s(b, "conv")) }),
        "agent-prompt" => json!({ "blocks": agent_blocks(hub) }),
        "agent-typing" => { *c.agent_busy.lock().unwrap() = if b["on"] == json!(true) { Some((s(b, "target"), s(b, "conv"))) } else { None }; c.room_changed(); json!({ "ok": true }) }
        "has-key" => json!({ "ok": c.has_key(&s(b, "user")) }),
        "changed" => { if b["keys"] == json!(true) { c.keys_changed(); } else { c.room_changed(); } json!({ "ok": true }) }
        _ => json!({ "ok": false, "error": "unknown" }),
    }
}

/// The team tools of a Work run (bridge/team.js): post as "<person> · Claude", who is here, servers, the wider picture.
pub fn team_tool(hub: &Arc<Hub>, name: &str, args: &Value, a: &Account) -> Value {
    let c = &hub.mods.chat; let us = users(hub); let u = a.username.as_str();
    let ok = |v: Value| json!({ "ok": true, "result": v }); let no = |e: String| json!({ "ok": false, "error": e });
    let disp = |x: &str| hub.mods.name_of(x);
    let now = now_ms() as i64;
    let people = || -> Vec<Value> { c.presence_list(&us).into_iter().map(|p| { let seen = p["seen"].as_i64().unwrap_or(0); json!({ "user": p["user"], "name": disp(&s(&p, "user")), "state": if p["online"] == json!(true) { if p["idle"] == json!(true) { "away" } else { "online" } } else { "offline" }, "screen": s(&p, "screen"), "lastSeen": if seen > 0 { json!(guilds_iso(seen)) } else { Value::Null }, "agoMin": if seen > 0 { json!(((now - seen) as f64 / 60000.0).round() as i64) } else { Value::Null } }) }).collect() };
    match name {
        "team_channel_create" | "team_group_create" => {
            let ctx = ctx!(hub, us.clone());
            let (op, body) = if name == "team_channel_create" { ("createChannel", json!({ "server": if s(args, "server").is_empty() { "mills".to_string() } else { s(args, "server") }, "name": s(args, "name"), "topic": s(args, "topic") })) }
                else { ("createGroup", json!({ "name": s(args, "name"), "members": if args["members"].is_array() { args["members"].clone() } else { json!(s(args, "members").split(|ch: char| ch == ',' || ch.is_whitespace()).filter(|x| !x.is_empty()).collect::<Vec<_>>()) } })) };
            let (res, ann) = c.guilds.op(op, u, &body, &ctx);
            for (conv, text) in ann { agent_say(hub, &text, "", "VibeRoller", "", &conv); }
            match res {
                Ok(v) => { c.room_changed(); if op == "createChannel" { ok(json!({ "created": true, "conv": format!("ch:{}/{}", s(&body, "server"), s(&v, "id")), "channel": v })) } else { ok(json!({ "created": true, "conv": format!("grp:{}", s(&v, "id")), "group": { "id": v["id"], "name": v["name"], "members": v["members"] } })) } }
                Err(e) => no(e),
            }
        }
        "hub_status" => {
            let calls = calls_state(hub, u, &us);
            let in_calls: Vec<Value> = calls.as_object().cloned().unwrap_or_default().into_iter().map(|(conv, ps)| json!({ "conv": conv, "people": ps.as_array().cloned().unwrap_or_default().iter().map(|p| json!({ "user": p["user"], "name": disp(&s(p, "user")), "cam": p["cam"], "mic": p["mic"], "screen": p["screen"] })).collect::<Vec<_>>() })).collect();
            let shares: Vec<Value> = hub.mods.work.shares_for(u).into_iter().map(|x| json!({ "user": x["user"], "name": disp(&s(&x, "user")), "since": x["since"] })).collect();
            let usage = hub.mods.work.usage_status(hub, a);
            let fmt = |w: &Value| if !w.is_object() { Value::Null } else { json!({ "source": w["source"], "status": w["status"], "usedPercent": w["utilization"].as_f64().map(|x| json!((x * 100.0).round() as i64)).unwrap_or(Value::Null), "resetsInMin": w["resetsAt"].as_f64().map(|r| json!(((r - now as f64) / 60000.0).round().max(0.0) as i64)).unwrap_or(Value::Null) }) };
            let mins = |t: &Value| t.as_str().and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok()).map(|d| json!(((now - d.timestamp_millis()) as f64 / 60000.0).round() as i64)).unwrap_or(Value::Null);
            let hosts: Vec<Value> = hub.mods.ai.status().into_iter().map(|h| json!({ "nickname": h["nickname"], "owner": if s(&h, "user").is_empty() { String::new() } else { disp(&s(&h, "user")) }, "state": if h["online"] == json!(true) { "up" } else { "down" }, "sinceMin": mins(&h["since"]), "lastSeenMin": mins(&h["seen"]), "version": s(&h, "version"), "offers": h["providers"] })).collect();
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
            for (x, _, _) in &us { if !list.iter().any(|p| p["user"] == json!(x)) { list.push(json!({ "user": x, "name": disp(x), "state": "offline", "screen": "", "lastSeen": Value::Null, "agoMin": Value::Null })); } }
            ok(json!({ "you": u, "people": list }))
        }
        _ => no(format!("Unknown team tool: {name}")),
    }
}
fn guilds_iso(ms: i64) -> String { super::auth::iso_at(ms) }
