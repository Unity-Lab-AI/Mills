//! Servers ("guilds"), channels and group conversations (moved from bridge/guilds.js): the Discord-shaped side of the
//! chat. Conversation ids: 'room' (#general of the everyone-server "VibeRoller"), 'dm:<a>|<b>', 'ch:<server>/<channel>',
//! 'grp:<id>'. Membership decides who may get a key slot, read, type, mark seen, call and be posted to; Unity counts as a
//! member everywhere. Roles + permissions: perms.rs. Storage: Chat/guilds.json (the same file and shape).
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::perms::{self, Perms};

const GROUP_MAX: usize = 10; const CHANNEL_MAX: usize = 50; const ROLE_MAX: usize = 25; const INVITE_MAX: usize = 50; const AUDIT_KEEP: usize = 200; const SERVER_MAX_PER_USER: usize = 25; const PIN_MAX: usize = 50; const GROUPS_PER_USER: usize = 100;
const INVITE_AGES: [i64; 7] = [0, 30, 60, 360, 720, 1440, 10080];
const TIMEOUTS: [i64; 7] = [1, 5, 10, 60, 1440, 10080, 40320];
const SLOWMODES: [i64; 14] = [0, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 21600];
const BAN_DELETE: [i64; 4] = [0, 3600, 86400, 604800];
const WAITS: [i64; 3] = [0, 5, 10];
const PW_TRIES: usize = 5; const PW_WINDOW: u64 = 600_000; const TEMP_GRACE: u64 = 60_000;
const SELF_SAFE: [&str; 8] = ["VIEW_CHANNEL", "CHANGE_NICKNAME", "SEND_MESSAGES", "SEND_VOICE_MESSAGES", "READ_MESSAGE_HISTORY", "CONNECT", "SPEAK", "STREAM"];

/// Who there is (the accounts as they are now) and what the chat knows, for one call.
pub struct Ctx<'a> {
    pub users: &'a [(String, String, bool)], // username, display, admin
    pub seq: i64,
    pub msg_conv: &'a (dyn Fn(i64) -> Option<String> + Sync),
    pub delete_recent: &'a (dyn Fn(&str, &[String], i64) -> usize + Sync),
}
impl Ctx<'_> {
    fn has(&self, u: &str) -> bool { self.users.iter().any(|x| x.0 == u) }
    fn name(&self, u: &str) -> String { self.users.iter().find(|x| x.0 == u).map(|x| if x.1.is_empty() { u.to_string() } else { x.1.clone() }).unwrap_or_else(|| u.to_string()) }
    fn admin(&self, u: &str) -> bool { self.users.iter().any(|x| x.0 == u && x.2) }
}
/// What an op leaves to do once the servers are saved: welcome-channel lines posted as "VibeRoller".
pub type Announce = Vec<(String, String)>;

pub struct Guilds { file: PathBuf, db: Mutex<Value>, ver: AtomicU64, last_post: Mutex<HashMap<String, u64>>, pw_tries: Mutex<HashMap<String, Vec<u64>>>, pub agent_name: String }
fn obj(v: &Value) -> Value { if v.is_object() { v.clone() } else { json!({}) } }
fn arr(v: &Value) -> Vec<Value> { v.as_array().cloned().unwrap_or_default() }
fn strs(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }
fn s(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_string() }
fn truthy(v: &Value) -> bool { !(v.is_null() || *v == json!(false) || *v == json!(0) || *v == json!("")) }
fn now() -> u64 { vr_core::now_ms() }
fn iso() -> String { super::auth::iso_now() }
fn iso_at(ms: i64) -> String { super::auth::iso_at(ms) }
fn ms_of(t: &Value) -> Option<i64> { t.as_str().and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok()).map(|d| d.timestamp_millis()) }
pub fn slug(x: &str) -> String {
    let low = x.to_lowercase(); let t = low.trim(); let mut out = String::new(); let mut dash = false;
    for c in t.chars() { if c.is_ascii_lowercase() || c.is_ascii_digit() { out.push(c); dash = false; } else if !dash { out.push('-'); dash = true; } }
    let out = out.trim_matches('-').to_string(); out.chars().take(32).collect()
}
/// At most `max` UTF-16 units, as JavaScript's slice counts them.
pub fn take_units(t: &str, max: usize) -> String { let mut n = 0; t.chars().take_while(|c| { n += c.len_utf16(); n <= max }).collect() }
fn clean_name(v: &Value, max: usize) -> String { let t = match v { Value::String(x) => x.clone(), Value::Null => String::new(), o => o.to_string() }; take_units(t.chars().filter(|c| !((*c as u32) < 0x20 || *c as u32 == 0x7f)).collect::<String>().trim(), max) }
fn color_ok(c: &str) -> bool { c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()) }
fn num(v: &Value) -> Option<i64> { v.as_f64().map(|f| f as i64).or_else(|| v.as_str().and_then(|x| x.trim().parse::<f64>().ok()).map(|f| f as i64)).or(if v.is_null() { None } else if *v == json!(true) { Some(1) } else if *v == json!(false) { Some(0) } else { None }) }
fn hexid(n: usize) -> String { vr_core::crypto::random_hex(n) }
fn perm_name(p: &str) -> String { p.to_lowercase().replace('_', " ") }
type R<T> = Result<T, String>;

/// A conversation id, taken apart.
pub enum Conv { Room, Dm(String, String), Ch(String, String), Grp(String) }
pub fn parse(conv: &str) -> Option<Conv> {
    let ok = |x: &str| !x.is_empty() && x.len() <= 32 && x.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if conv == "room" { return Some(Conv::Room); }
    if let Some(r) = conv.strip_prefix("dm:") { let (a, b) = r.split_once('|')?; return (ok(a) && ok(b)).then(|| Conv::Dm(a.into(), b.into())); }
    if let Some(r) = conv.strip_prefix("ch:") { let (a, b) = r.split_once('/')?; return (ok(a) && ok(b)).then(|| Conv::Ch(a.into(), b.into())); }
    if let Some(g) = conv.strip_prefix("grp:") { return ok(g).then(|| Conv::Grp(g.into())); }
    None
}
pub fn is_guild_conv(conv: &str) -> bool { conv.starts_with("ch:") || conv.starts_with("grp:") }

impl Guilds {
    pub fn new(chat_dir: &std::path::Path, seed: PathBuf, agent_name: &str, users: &[(String, String, bool)]) -> Guilds {
        let file = chat_dir.join("guilds.json");
        let mut db: Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        if !db["servers"].is_array() { db = json!({ "servers": [], "groups": [] }); }
        if !db["groups"].is_array() { db["groups"] = json!([]); }
        if !db["pins"].is_object() { db["pins"] = json!({}); }
        let has_mills = db["servers"].as_array().unwrap().iter().any(|x| x["id"] == json!("mills"));
        if !has_mills { db["servers"].as_array_mut().unwrap().insert(0, json!({ "id": "mills", "name": "VibeRoller", "everyone": true, "channels": [{ "id": "general", "name": "general", "topic": "the whole team" }], "createdBy": "" })); }
        for sv in db["servers"].as_array_mut().unwrap() {
            if sv["id"] == json!("mills") { sv["everyone"] = json!(true); if sv["name"] == json!("Mills") { sv["name"] = json!("VibeRoller"); } }
            norm_server(sv);
        }
        for g in db["groups"].as_array_mut().unwrap() { if s(g, "owner").is_empty() { g["owner"] = json!(if !s(g, "createdBy").is_empty() { s(g, "createdBy") } else { strs(&g["members"]).first().cloned().unwrap_or_default() }); } }
        let gl = Guilds { file, db: Mutex::new(db), ver: AtomicU64::new(1), last_post: Mutex::default(), pw_tries: Mutex::default(), agent_name: agent_name.into() };
        gl.apply_seeds(&seed, users);
        gl
    }
    pub fn version(&self) -> u64 { self.ver.load(Ordering::SeqCst) }
    fn write(&self, db: &Value) { let tmp = self.file.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(db).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp, &self.file); } }
    fn save(&self, db: &Value) { self.write(db); self.ver.fetch_add(1, Ordering::SeqCst); }
    /// bridge/server-owners.json: each server's entry applied once (its exact text is remembered).
    fn apply_seeds(&self, seed: &std::path::Path, users: &[(String, String, bool)]) {
        let Some(sd) = std::fs::read_to_string(seed).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return };
        let Some(servers) = sd["servers"].as_object() else { return };
        let mut db = self.db.lock().unwrap(); if !db["seeds"].is_object() { db["seeds"] = json!({}); }
        let has = |u: &str| users.iter().any(|x| x.0 == u);
        let mut any = false;
        for (id, e) in servers {
            let key = if e.is_null() { "{}".to_string() } else { e.to_string() };
            if !e.is_object() || db["seeds"][id] == json!(key) { continue; }
            let Some(i) = db["servers"].as_array().unwrap().iter().position(|x| x["id"] == json!(id)) else { continue };
            let sv = &mut db["servers"][i];
            let owner = s(e, "owner").to_lowercase();
            let co = sv["roles"].as_array().unwrap().iter().find(|r| r["builtin"] == json!("coowner")).map(|r| s(r, "id"));
            if !owner.is_empty() && has(&owner) {
                if sv["everyone"] != json!(true) && !strs(&sv["members"]).contains(&owner) { sv["members"].as_array_mut().unwrap().push(json!(owner)); }
                sv["owner"] = json!(owner);
                if let Some(co) = &co { if sv["memberRoles"][&owner].is_array() { let l: Vec<String> = strs(&sv["memberRoles"][&owner]).into_iter().filter(|x| x != co).collect(); sv["memberRoles"][&owner] = json!(l); } }
            }
            for u0 in strs(&e["coOwners"]) {
                let u = u0.to_lowercase();
                let Some(co) = &co else { continue };
                if !has(&u) || u == s(sv, "owner") || (sv["everyone"] != json!(true) && !strs(&sv["members"]).contains(&u)) { continue; }
                let mut l = strs(&sv["memberRoles"][&u]); if !l.contains(co) { l.push(co.clone()); } sv["memberRoles"][&u] = json!(l);
            }
            let cos = strs(&e["coOwners"]);
            let detail = [if owner.is_empty() { String::new() } else { format!("owner {owner}") }, if cos.is_empty() { String::new() } else { format!("co-owners {}", cos.join(", ")) }].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("; ");
            audit(sv, "host", "set the owner and co-owners", &detail);
            db["seeds"][id] = json!(key); any = true;
        }
        if any { self.write(&db); }
    }
    pub fn with<T>(&self, f: impl FnOnce(&Value) -> T) -> T { let db = self.db.lock().unwrap(); f(&db) }

    // ---------- reading (what the chat and calls ask) ----------
    pub fn chan_of<'v>(db: &'v Value, conv: &str) -> Option<(&'v Value, &'v Value)> {
        match parse(conv)? {
            Conv::Room => { let sv = db["servers"].as_array()?.iter().find(|x| x["id"] == json!("mills"))?; let ch = sv["channels"].as_array()?.iter().find(|c| c["id"] == json!("general"))?; Some((sv, ch)) }
            Conv::Ch(a, b) => { let sv = db["servers"].as_array()?.iter().find(|x| x["id"] == json!(a))?; let ch = sv["channels"].as_array()?.iter().find(|c| c["id"] == json!(b))?; if sv["everyone"] == json!(true) && ch["id"] == json!("general") { return None; } Some((sv, ch)) }
            _ => None,
        }
    }
    fn server_members(sv: &Value, ctx: &Ctx) -> Vec<String> { if sv["everyone"] == json!(true) { ctx.users.iter().map(|x| x.0.clone()).collect() } else { strs(&sv["members"]).into_iter().filter(|u| ctx.has(u)).collect() } }
    fn in_server(sv: &Value, u: &str, ctx: &Ctx) -> bool { Self::server_members(sv, ctx).iter().any(|x| x == u) }
    pub fn exists(&self, conv: &str, ctx: &Ctx) -> bool {
        let db = self.db.lock().unwrap();
        match parse(conv) { None => false, Some(Conv::Room) => true, Some(Conv::Dm(a, b)) => a < b && ctx.has(&a) && ctx.has(&b), Some(Conv::Ch(..)) => Self::chan_of(&db, conv).is_some(), Some(Conv::Grp(g)) => db["groups"].as_array().unwrap().iter().any(|x| x["id"] == json!(g)) }
    }
    /// The people of a channel / group conversation (None for room / dm / unknown).
    pub fn members(&self, conv: &str, ctx: &Ctx) -> Option<Vec<String>> { let db = self.db.lock().unwrap(); Self::members_in(&db, conv, ctx) }
    fn members_in(db: &Value, conv: &str, ctx: &Ctx) -> Option<Vec<String>> {
        let p = Perms { hub_admin: &|u| ctx.admin(u) };
        match parse(conv)? {
            Conv::Ch(..) => { let (sv, ch) = Self::chan_of(db, conv)?; Some(Self::server_members(sv, ctx).into_iter().filter(|u| p.can_in(sv, Some(ch), u, "VIEW_CHANNEL")).collect()) }
            Conv::Grp(g) => { let gr = db["groups"].as_array()?.iter().find(|x| x["id"] == json!(g))?; Some(strs(&gr["members"]).into_iter().filter(|u| ctx.has(u)).collect()) }
            _ => None,
        }
    }
    pub fn is_member(&self, conv: &str, u: &str, ctx: &Ctx) -> bool { self.members(conv, ctx).map(|l| l.iter().any(|x| x == u)).unwrap_or(false) }
    pub fn allows(&self, conv: &str, u: &str, perm: &str, ctx: &Ctx) -> bool { let db = self.db.lock().unwrap(); let p = Perms { hub_admin: &|x| ctx.admin(x) }; match Self::chan_of(&db, conv) { Some((sv, ch)) => p.can_in(sv, Some(ch), u, perm), None => true } }
    /// '' = may post there; otherwise why not (timeout, permissions, the new-member wait, slowmode).
    pub fn post_check(&self, conv: &str, u: &str, voice: bool, edit: bool, ctx: &Ctx) -> String {
        let db = self.db.lock().unwrap(); let p = Perms { hub_admin: &|x| ctx.admin(x) };
        let Some((sv, ch)) = Self::chan_of(&db, conv) else { return String::new() };
        let perms = p.perms_in(sv, Some(ch), u); let has = |x: &str| perms.iter().any(|y| y == x);
        if p.timed_out(sv, u) { let t = ms_of(&sv["timeouts"][u]).unwrap_or(0); return format!("You are timed out in {} until {}: you can read but not post.", s(sv, "name"), local_time(t)); }
        if !has("SEND_MESSAGES") { return if ch["announce"] == json!(true) { format!("#{} is an announcement channel: only people with the announce permission post here.", s(ch, "name")) } else { format!("You may not post in #{}.", s(ch, "name")) }; }
        if voice && !has("SEND_VOICE_MESSAGES") { return format!("You may not send voice clips in #{}.", s(ch, "name")); }
        if edit { return String::new(); }
        let wait = sv["settings"]["wait"].as_i64().unwrap_or(0); let j = &sv["joined"][u];
        if wait > 0 && j.is_object() && real_roles(sv, u).is_empty() { let left = wait * 60000 - (now() as i64 - j["at"].as_i64().unwrap_or(0)); if left > 0 { return format!("New members of {} wait {wait} minutes before posting ({} to go).", s(sv, "name"), (left as f64 / 60000.0).ceil() as i64); } }
        let slow = ch["slowmode"].as_i64().unwrap_or(0);
        if slow > 0 && !has("BYPASS_SLOWMODE") && !has("MANAGE_MESSAGES") && !has("MANAGE_CHANNELS") {
            let last = self.last_post.lock().unwrap().get(&format!("{conv}|{u}")).copied().unwrap_or(0); let left = slow * 1000 - (now() - last) as i64;
            if left > 0 { return format!("Slowmode in #{}: wait {} s.", s(ch, "name"), (left as f64 / 1000.0).ceil() as i64); }
        }
        String::new()
    }
    /// A post went in (slowmode counts from here).
    pub fn note_post_by(&self, conv: &str, u: &str) { let ok = { let db = self.db.lock().unwrap(); Self::chan_of(&db, conv).is_some() }; if ok { self.last_post.lock().unwrap().insert(format!("{conv}|{u}"), now()); } }
    /// READ_MESSAGE_HISTORY: without it only what arrived after they joined.
    pub fn can_read(&self, conv: &str, u: &str, seq: i64, ctx: &Ctx) -> bool {
        let db = self.db.lock().unwrap(); let p = Perms { hub_admin: &|x| ctx.admin(x) };
        let Some((sv, ch)) = Self::chan_of(&db, conv) else { return true };
        if p.can_in(sv, Some(ch), u, "READ_MESSAGE_HISTORY") { return true; }
        let j = &sv["joined"][u]; j.is_object() && seq > j["seq"].as_i64().unwrap_or(0)
    }
    /// What a person may do in this conversation's call.
    pub fn media(&self, conv: &str, u: &str, ctx: &Ctx) -> Value {
        let db = self.db.lock().unwrap(); let p = Perms { hub_admin: &|x| ctx.admin(x) };
        let Some((sv, ch)) = Self::chan_of(&db, conv) else { return json!({ "connect": true, "speak": true, "stream": true, "muted": false }) };
        let perms = p.perms_in(sv, Some(ch), u); let has = |x: &str| perms.iter().any(|y| y == x);
        json!({ "connect": has("CONNECT"), "speak": has("SPEAK"), "stream": has("STREAM"), "muted": truthy(&sv["voiceMuted"][u]) })
    }
    pub fn moderates(&self, conv: &str, u: &str, ctx: &Ctx) -> bool { let ok = { let db = self.db.lock().unwrap(); Self::chan_of(&db, conv).is_some() }; ok && self.allows(conv, u, "MANAGE_MESSAGES", ctx) }

    // ---------- what one person sees ----------
    pub fn view(&self, user: &str, ctx: &Ctx) -> Value { let db = self.db.lock().unwrap(); self.view_in(&db, user, ctx) }
    fn view_in(&self, db: &Value, user: &str, ctx: &Ctx) -> Value {
        let p = Perms { hub_admin: &|x| ctx.admin(x) };
        let pins_of = |conv: &str| -> Value { json!(arr(&db["pins"][conv]).into_iter().filter(|pn| (ctx.msg_conv)(pn["seq"].as_i64().unwrap_or(0)).is_some()).collect::<Vec<_>>()) };
        let is_private = |ch: &Value| arr(&ch["overwrites"]).iter().any(|o| o["type"] == json!("role") && o["id"] == json!("everyone") && strs(&o["deny"]).iter().any(|x| x == "VIEW_CHANNEL"));
        let now_ms = now() as i64;
        let servers: Vec<Value> = arr(&db["servers"]).iter().filter(|sv| Self::in_server(sv, user, ctx)).map(|sv| {
            let perms = p.perms_of(sv, user); let has = |x: &str| perms.iter().any(|y| y == x); let manage = has("MANAGE_CHANNELS") || has("MANAGE_ROLES");
            let mut roles = arr(&sv["roles"]); roles.sort_by(|a, b| b["position"].as_f64().unwrap_or(0.0).partial_cmp(&a["position"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
            let mut chans = arr(&sv["channels"]); chans.sort_by(|a, b| a["position"].as_f64().unwrap_or(0.0).partial_cmp(&b["position"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
            let rank = p.rank(sv, user);
            let mut v = json!({ "id": sv["id"], "name": sv["name"], "icon": sv["icon"], "color": sv["color"], "description": sv["description"], "everyone": sv["everyone"] == json!(true), "owner": s(sv, "owner"), "createdBy": s(sv, "createdBy"),
                "locked": truthy(&sv["password"]), "settings": sv["settings"], "members": Self::server_members(sv, ctx), "temp": sv["temp"].as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(), "nicks": sv["nicks"],
                "timeouts": Value::Object(sv["timeouts"].as_object().cloned().unwrap_or_default().into_iter().filter(|(u, t)| ms_of(t).map(|x| x > now_ms).unwrap_or(false) && (u == user || has("MODERATE_MEMBERS"))).collect()),
                "voiceMuted": sv["voiceMuted"].as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                "me": { "perms": perms, "roles": if sv["memberRoles"][user].is_array() { sv["memberRoles"][user].clone() } else { json!([]) }, "owner": p.is_owner(sv, user), "admin": p.is_admin(sv, user), "rank": if rank.is_infinite() { json!(1e9 as i64) } else { json!(rank as i64) } },
                "roles": roles, "memberRoles": sv["memberRoles"], "everyonePerms": sv["everyonePerms"],
                "channels": chans.iter().filter(|ch| p.can_in(sv, Some(ch), user, "VIEW_CHANNEL")).map(|ch| {
                    let conv = if sv["everyone"] == json!(true) && ch["id"] == json!("general") { "room".to_string() } else { format!("ch:{}/{}", s(sv, "id"), s(ch, "id")) };
                    let mine = p.perms_in(sv, Some(ch), user);
                    let mut c = json!({ "id": ch["id"], "name": ch["name"], "topic": ch["topic"], "category": ch["category"], "position": ch["position"], "private": is_private(ch), "announce": ch["announce"], "nsfw": ch["nsfw"], "slowmode": ch["slowmode"], "createdBy": ch["createdBy"],
                        "perms": mine, "canPost": mine.iter().any(|x| x == "SEND_MESSAGES"), "conv": conv, "pins": pins_of(&conv) });
                    if is_private(ch) { c["members"] = json!(Self::server_members(sv, ctx).into_iter().filter(|u| p.can_in(sv, Some(ch), u, "VIEW_CHANNEL")).collect::<Vec<_>>()); }
                    if manage || ch["createdBy"] == json!(user) { c["overwrites"] = ch["overwrites"].clone(); }
                    c
                }).collect::<Vec<_>>() });
            if has("CREATE_INVITE") || has("MANAGE_INVITES") {
                v["invites"] = json!(arr(&sv["invites"]).iter().filter(|iv| live(iv) && (has("MANAGE_INVITES") || iv["by"] == json!(user))).map(pub_invite).collect::<Vec<_>>());
                v["pending"] = json!(sv["pending"].as_object().map(|m| m.iter().map(|(u, pd)| json!({ "user": u, "by": pd["by"], "at": pd["at"] })).collect::<Vec<_>>()).unwrap_or_default());
            }
            if has("BAN_MEMBERS") { v["bans"] = json!(arr(&sv["bans"]).iter().map(|b| json!({ "user": b["user"], "by": b["by"], "at": b["at"], "reason": s(b, "reason") })).collect::<Vec<_>>()); }
            if has("VIEW_AUDIT_LOG") { let a = arr(&sv["audit"]); v["audit"] = json!(a.iter().skip(a.len().saturating_sub(100)).rev().cloned().collect::<Vec<_>>()); }
            v
        }).collect();
        let invitations: Vec<Value> = arr(&db["servers"]).iter().filter(|sv| sv["everyone"] != json!(true) && sv["pending"][user].is_object() && !Self::in_server(sv, user, ctx)).map(|sv| json!({ "server": sv["id"], "name": sv["name"], "icon": sv["icon"], "color": sv["color"], "by": sv["pending"][user]["by"], "at": sv["pending"][user]["at"], "members": Self::server_members(sv, ctx).len() })).collect();
        let groups: Vec<Value> = arr(&db["groups"]).iter().filter(|g| strs(&g["members"]).iter().any(|x| x == user)).map(|g| { let conv = format!("grp:{}", s(g, "id")); json!({ "id": g["id"], "name": g["name"], "members": strs(&g["members"]).into_iter().filter(|u| ctx.has(u)).collect::<Vec<_>>(), "owner": g["owner"], "createdBy": g["createdBy"], "conv": conv, "pins": pins_of(&conv) }) }).collect();
        let dm_pins: Map<String, Value> = db["pins"].as_object().map(|m| m.keys().filter(|c| c.starts_with("dm:") && c[3..].split('|').any(|x| x == user)).map(|c| (c.clone(), pins_of(c))).collect()).unwrap_or_default();
        json!({ "ver": self.version(), "servers": servers, "groups": groups, "invitations": invitations, "dmPins": dm_pins, "agent": self.agent_name, "perms": perms::perms_json(), "categories": perms::categories_json(), "channelPerms": perms::CHANNEL_PERMS,
            "templates": perms::templates(), "groupMax": GROUP_MAX, "inviteAges": INVITE_AGES, "timeouts": TIMEOUTS, "slowmodes": SLOWMODES, "banDelete": BAN_DELETE, "waits": WAITS })
    }

    /// One op of the /api/guilds/* routes (or a team tool): its result, or why not. Announcements to post afterwards.
    pub fn op(&self, name: &str, user: &str, b: &Value, ctx: &Ctx) -> (R<Value>, Announce) {
        let mut db = self.db.lock().unwrap();
        let mut ann: Announce = vec![];
        let before = db.clone();
        let res = self.op_in(&mut db, name, user, b, ctx, &mut ann);
        // saved (and the version moved) after every op as the JavaScript hub did, except the ones that only look
        match &res { Ok(_) => { if *db != before || !matches!(name, "previewInvite" | "declineInvitation" | "cancelInvitation" | "moveChannel") { self.save(&db); } } Err(_) => { *db = before; ann.clear(); } }
        (res, ann)
    }
    fn op_in(&self, db: &mut Value, name: &str, user: &str, b: &Value, ctx: &Ctx, ann: &mut Announce) -> R<Value> {
        let p = Perms { hub_admin: &|x| ctx.admin(x) };
        let sid = s(b, "server");
        let find = |db: &Value, id: &str| db["servers"].as_array().unwrap().iter().position(|x| x["id"] == json!(id));
        let member = |db: &Value, id: &str| -> R<usize> { let i = find(db, id).ok_or("No such server.")?; if !Self::in_server(&db["servers"][i], user, ctx) { return Err("You are not in that server.".into()); } Ok(i) };
        let need = |db: &Value, id: &str, perm: &str| -> R<usize> { let i = member(db, id)?; if !p.can(&db["servers"][i], user, perm) { return Err(format!("You need the \"{}\" permission in {}.", perm_name(perm), s(&db["servers"][i], "name"))); } Ok(i) };
        let owned = |db: &Value, id: &str| -> R<usize> { let i = member(db, id)?; let sv = &db["servers"][i]; if sv["everyone"] == json!(true) { return Err("The VibeRoller server is everyone's.".into()); } if sv["owner"] != json!(user) { return Err("Only the server's owner may do that.".into()); } Ok(i) };
        let not_everyone = |sv: &Value, what: &str| -> R<()> { if sv["everyone"] == json!(true) { Err(format!("Everyone is always in the VibeRoller server, so it has no {what}.")) } else { Ok(()) } };
        let target = |sv: &Value, who: &str, perm: Option<&str>| -> R<String> {
            let who = who.to_lowercase();
            if !ctx.has(&who) { return Err("No such person.".into()); }
            if who == user { return Err("Not on yourself.".into()); }
            if p.is_owner(sv, &who) { return Err("Not on the server's owner.".into()); }
            if let Some(pm) = perm { if !p.can(sv, user, pm) { return Err(format!("You need the \"{}\" permission.", perm_name(pm))); } }
            if !p.outranks(sv, user, &who) { return Err(format!("{} is not below you in {}'s roles.", ctx.name(&who), s(sv, "name"))); }
            Ok(who)
        };
        let user_list = |v: &Value| -> Vec<String> { let raw: Vec<String> = match v { Value::Array(a) => a.iter().map(|x| match x { Value::String(t) => t.clone(), o => o.to_string() }).collect(), Value::String(t) => t.split(|c: char| c == ',' || c.is_whitespace()).map(String::from).collect(), Value::Null => vec![], o => vec![o.to_string()] }; let mut out: Vec<String> = vec![]; for u in raw { let u = u.trim().to_lowercase(); if ctx.has(&u) && !out.contains(&u) { out.push(u); } } out };
        let as_list = |v: &Value| if v.is_array() { v.clone() } else { json!([v]) };
        let sys_say = |sv: &Value, text: String, key: &str, ann: &mut Announce| { if sv["everyone"] == json!(true) || s(&sv["settings"], "systemChannel").is_empty() || !truthy(&sv["settings"][key]) { return; } let chid = s(&sv["settings"], "systemChannel"); if arr(&sv["channels"]).iter().any(|c| c["id"] == json!(chid)) { ann.push((format!("ch:{}/{}", s(sv, "id"), chid), text)); } };
        let join = |sv: &mut Value, who: &str, how: String, temporary: bool, ann: &mut Announce| -> R<()> {
            if arr(&sv["bans"]).iter().any(|x| x["user"] == json!(who)) { return Err(format!("You are banned from {}.", s(sv, "name"))); }
            if Self::in_server(sv, who, ctx) { return Ok(()); }
            if !strs(&sv["members"]).iter().any(|x| x == who) { sv["members"].as_array_mut().unwrap().push(json!(who)); }
            if let Some(m) = sv["pending"].as_object_mut() { m.remove(who); }
            sv["joined"][who] = json!({ "at": now(), "seq": ctx.seq });
            if temporary { sv["temp"][who] = json!(now()); } else if let Some(mid) = member_role(sv) { let mut l = vec![mid.clone()]; for x in strs(&sv["memberRoles"][who]) { if !l.contains(&x) { l.push(x); } } sv["memberRoles"][who] = json!(l); }
            audit(sv, who, "joined", &format!("{how}{}", if temporary { " (temporary)" } else { "" }));
            let rules = s(&sv["settings"], "rulesChannel");
            let rules_txt = if rules.is_empty() { String::new() } else { format!(" Read #{} first.", arr(&sv["channels"]).iter().find(|c| c["id"] == json!(rules)).map(|c| s(c, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| "rules".into())) };
            sys_say(sv, format!("{} joined {}. Welcome!{rules_txt}", ctx.name(who), s(sv, "name")), "systemJoins", ann);
            Ok(())
        };
        let find_invite = |db: &Value, code: &Value| -> Option<(usize, usize)> {
            let c0 = match code { Value::String(t) => t.clone(), Value::Null => String::new(), o => o.to_string() }; let c0 = c0.trim();
            let c1 = match c0.rfind("invite=") { Some(i) if c0[..i].ends_with('?') || c0[..i].ends_with('&') => &c0[i + 7..], _ => c0 };
            let code: String = c1.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            for (i, sv) in db["servers"].as_array().unwrap().iter().enumerate() { if let Some(j) = arr(&sv["invites"]).iter().position(|x| x["code"] == json!(code)) { return Some((i, j)); } }
            None
        };
        match name {
            "createServer" => {
                let nm = clean_name(&b["name"], 40); if nm.is_empty() { return Err("A server needs a name.".into()); }
                if arr(&db["servers"]).iter().filter(|x| x["owner"] == json!(user)).count() >= SERVER_MAX_PER_USER { return Err("You own enough servers already.".into()); }
                let mut id = slug(&nm); if id.is_empty() { id = "s".into(); }
                if id == "mills" || find(db, &id).is_some() { id = format!("{}-{}", id.chars().take(27).collect::<String>(), hexid(2)); }
                let mut sv = json!({ "id": id, "name": nm, "icon": clean_name(&b["icon"], 2), "color": b["color"], "description": clean_name(&b["description"], 300), "channels": [{ "id": "general", "name": "general", "topic": "" }], "members": [user], "owner": user, "createdBy": user, "createdAt": iso(), "permsVersion": 2 });
                norm_server(&mut sv);
                sv["joined"][user] = json!({ "at": now(), "seq": 0 }); sv["memberRoles"][user] = json!([member_role(&sv).unwrap_or_default()]);
                audit(&mut sv, user, "created the server", "");
                for u in user_list(&b["members"]) { if u != user { sv["pending"][&u] = json!({ "by": user, "at": iso() }); let n = ctx.name(&u); audit(&mut sv, user, "invited", &n); } }
                db["servers"].as_array_mut().unwrap().push(sv.clone()); Ok(sv)
            }
            "updateServer" | "renameServer" => {
                let i = need(db, &sid, "MANAGE_SERVER")?; let mut ch: Vec<&str> = vec![];
                let sv = &mut db["servers"][i];
                if !b["name"].is_null() { let n = clean_name(&b["name"], 40); if !n.is_empty() && n != s(sv, "name") { sv["name"] = json!(n); ch.push("name"); } }
                if b.get("icon").is_some() { sv["icon"] = json!(clean_name(&b["icon"], 2)); ch.push("icon"); }
                if b.get("color").is_some() { sv["color"] = json!(if color_ok(&s(b, "color")) { s(b, "color") } else { String::new() }); ch.push("colour"); }
                if b.get("description").is_some() { sv["description"] = json!(clean_name(&b["description"], 300)); ch.push("description"); }
                if b["settings"].is_object() { if sv["everyone"] == json!(true) && (truthy(&b["settings"]["invitesPaused"]) || truthy(&b["settings"]["systemChannel"])) { return Err("The VibeRoller server has no invites or join messages.".into()); } clean_settings(sv, &b["settings"]); ch.push("settings"); }
                if b.get("everyonePerms").is_some() {
                    if !p.can(sv, user, "MANAGE_ROLES") { return Err("You need the \"manage roles\" permission.".into()); }
                    let perms: Vec<String> = perms::clean_perms(&b["everyonePerms"], &perms::all_perms()).into_iter().filter(|x| x != "ADMINISTRATOR").collect();
                    let cur = strs(&sv["everyonePerms"]);
                    let changed: Vec<String> = perms.iter().filter(|x| !cur.contains(x)).cloned().chain(cur.iter().filter(|x| !perms.contains(x)).cloned()).collect();
                    if !p.grantable(sv, user, &changed) { return Err("You can only change permissions you have yourself.".into()); }
                    sv["everyonePerms"] = json!(perms); ch.push("@everyone permissions");
                }
                if !ch.is_empty() { audit(sv, user, "changed the server", &ch.join(", ")); }
                Ok(sv.clone())
            }
            "setPassword" => {
                let i = need(db, &sid, "MANAGE_SERVER")?; let sv = &mut db["servers"][i]; not_everyone(sv, "password lock")?;
                let pw = match &b["password"] { Value::String(t) => t.clone(), Value::Null => String::new(), o => o.to_string() };
                if pw.is_empty() { sv["password"] = Value::Null; audit(sv, user, "removed the password lock", ""); }
                else {
                    let n = pw.encode_utf16().count(); if !(4..=200).contains(&n) { return Err("A server password is 4 to 200 characters.".into()); }
                    let salt = vr_core::crypto::random_bytes(16); let hash = scrypt32(&pw, &salt);
                    sv["password"] = json!({ "salt": vr_core::b64(&salt), "hash": vr_core::b64(&hash) }); audit(sv, user, "set a password lock", "");
                }
                Ok(json!({ "locked": truthy(&sv["password"]) }))
            }
            "transferServer" => {
                let i0 = find(db, &sid);
                let i = match i0 { Some(i) if db["servers"][i]["everyone"] == json!(true) => { if p.is_owner(&db["servers"][i], user) { i } else { owned(db, &sid)? } } _ => owned(db, &sid)? };
                let to = s(b, "user").to_lowercase();
                let sv = &mut db["servers"][i];
                if !Self::in_server(sv, &to, ctx) || to == user { return Err("Hand it to another member of the server.".into()); }
                let co = arr(&sv["roles"]).iter().find(|r| r["builtin"] == json!("coowner")).map(|r| s(r, "id"));
                sv["owner"] = json!(to); for k in ["temp", "timeouts", "voiceMuted"] { if let Some(m) = sv[k].as_object_mut() { m.remove(&to); } }
                let keep = b["keepCoOwner"] != json!(false);
                if let Some(co) = &co { let l: Vec<String> = strs(&sv["memberRoles"][&to]).into_iter().filter(|x| x != co).collect(); sv["memberRoles"][&to] = json!(l); if keep { let mut m = strs(&sv["memberRoles"][user]); if !m.contains(co) { m.push(co.clone()); } sv["memberRoles"][user] = json!(m); } }
                let n = ctx.name(&to); audit(sv, user, "handed ownership to", &format!("{n}{}", if co.is_some() && keep { " (stays on as Co-owner)" } else { "" }));
                Ok(sv.clone())
            }
            "leaveServer" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i]; not_everyone(sv, "leaving")?;
                if sv["owner"] == json!(user) { return Err("Hand the server to someone else first (or delete it).".into()); }
                remove_from(sv, user); audit(sv, user, "left", "");
                sys_say(sv, format!("{} left {}.", ctx.name(user), s(sv, "name")), "systemLeaves", ann);
                maybe_drop(db, i); Ok(json!(true))
            }
            "deleteServer" => { let i = owned(db, &sid)?; let id = s(&db["servers"][i], "id"); db["servers"].as_array_mut().unwrap().remove(i); if let Some(m) = db["pins"].as_object_mut() { m.retain(|c, _| !c.starts_with(&format!("ch:{id}/"))); } Ok(json!(true)) }
            "invitePeople" | "addMembers" => {
                let i = need(db, &sid, "CREATE_INVITE")?; let sv = &mut db["servers"][i]; not_everyone(sv, "invitations")?;
                let list: Vec<String> = user_list(&as_list(&b["add"])).into_iter().filter(|u| !Self::in_server(sv, u, ctx)).collect();
                if list.is_empty() { return Err("Nobody new to invite (they are in already, or no such person).".into()); }
                for u in &list { if arr(&sv["bans"]).iter().any(|x| x["user"] == json!(u)) { return Err(format!("{} is banned from {}.", ctx.name(u), s(sv, "name"))); } sv["pending"][u] = json!({ "by": user, "at": iso() }); let n = ctx.name(u); audit(sv, user, "invited", &n); }
                Ok(json!({ "invited": list }))
            }
            "cancelInvitation" => {
                let i = need(db, &sid, "CREATE_INVITE")?; let sv = &mut db["servers"][i]; let u = s(b, "user").to_lowercase();
                if sv["pending"][&u].is_object() && (sv["pending"][&u]["by"] == json!(user) || p.can(sv, user, "MANAGE_INVITES")) { sv["pending"].as_object_mut().unwrap().remove(&u); let n = ctx.name(&u); audit(sv, user, "cancelled the invitation of", &n); }
                Ok(json!(true))
            }
            "acceptInvitation" => {
                let Some(i) = find(db, &sid).filter(|i| db["servers"][*i]["pending"][user].is_object()) else { return Err("No invitation to that server.".into()) };
                let sv = &mut db["servers"][i]; let by = s(&sv["pending"][user], "by");
                join(sv, user, format!("invited by {}", ctx.name(&by)), false, ann)?; Ok(sv.clone())
            }
            "declineInvitation" => { if let Some(i) = find(db, &sid) { if let Some(m) = db["servers"][i]["pending"].as_object_mut() { m.remove(user); } } Ok(json!(true)) }
            "kick" | "removeMember" if name == "kick" || s(b, "user").to_lowercase() != user => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i]; not_everyone(sv, "kicking")?;
                let who = target(sv, &s(b, "user"), Some("KICK_MEMBERS"))?;
                if !Self::in_server(sv, &who, ctx) { return Err("They are not in the server.".into()); }
                remove_from(sv, &who);
                let reason = if truthy(&b["reason"]) { format!(": {}", clean_name(&b["reason"], 120)) } else { String::new() };
                let n = ctx.name(&who); audit(sv, user, "kicked", &format!("{n}{reason}"));
                sys_say(sv, format!("{n} left {}.", s(sv, "name")), "systemLeaves", ann); Ok(json!(true))
            }
            "removeMember" => self.op_in(db, "leaveServer", user, b, ctx, ann),
            "ban" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i]; not_everyone(sv, "bans")?;
                let who = target(sv, &s(b, "user"), Some("BAN_MEMBERS"))?;
                let del = num(&b["deleteSeconds"]).filter(|d| BAN_DELETE.contains(d)).unwrap_or(0);
                remove_from(sv, &who); let mut bans: Vec<Value> = arr(&sv["bans"]).into_iter().filter(|x| x["user"] != json!(who)).collect();
                bans.push(json!({ "user": who, "by": user, "at": iso(), "reason": clean_name(&b["reason"], 120) })); sv["bans"] = json!(bans);
                let convs: Vec<String> = arr(&sv["channels"]).iter().map(|c| format!("ch:{}/{}", s(sv, "id"), s(c, "id"))).collect();
                let gone = if del > 0 { (ctx.delete_recent)(&who, &convs, now() as i64 - del * 1000) } else { 0 };
                let reason = if truthy(&b["reason"]) { format!(": {}", clean_name(&b["reason"], 120)) } else { String::new() };
                let n = ctx.name(&who); audit(sv, user, "banned", &format!("{n}{reason}{}", if gone > 0 { format!(" ({gone} recent message{} deleted)", if gone == 1 { "" } else { "s" }) } else { String::new() }));
                Ok(json!({ "deleted": gone }))
            }
            "unban" => { let i = need(db, &sid, "BAN_MEMBERS")?; let sv = &mut db["servers"][i]; let who = s(b, "user").to_lowercase(); let bans: Vec<Value> = arr(&sv["bans"]).into_iter().filter(|x| x["user"] != json!(who)).collect(); sv["bans"] = json!(bans); let n = ctx.name(&who); audit(sv, user, "unbanned", &n); Ok(json!(true)) }
            "timeout" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i]; let who = target(sv, &s(b, "user"), Some("MODERATE_MEMBERS"))?;
                if !Self::in_server(sv, &who, ctx) { return Err("They are not in the server.".into()); }
                if p.is_admin(sv, &who) { return Err("Administrators cannot be timed out.".into()); }
                let min = num(&b["minutes"]).unwrap_or(0);
                if min == 0 { if let Some(m) = sv["timeouts"].as_object_mut() { m.remove(&who); } let n = ctx.name(&who); audit(sv, user, "removed the timeout of", &n); }
                else {
                    if !TIMEOUTS.contains(&min) { return Err("Pick a timeout length from the list.".into()); }
                    sv["timeouts"][&who] = json!(iso_at(now() as i64 + min * 60000));
                    let len = if min >= 1440 { format!("{} d", fmt_num(min as f64 / 1440.0)) } else if min >= 60 { format!("{} h", fmt_num(min as f64 / 60.0)) } else { format!("{min} min") };
                    let reason = if truthy(&b["reason"]) { format!(": {}", clean_name(&b["reason"], 120)) } else { String::new() };
                    let n = ctx.name(&who); audit(sv, user, "timed out", &format!("{n} for {len}{reason}"));
                }
                Ok(json!({ "until": sv["timeouts"][&who].as_str().unwrap_or("") }))
            }
            "voiceMute" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i]; let who = target(sv, &s(b, "user"), Some("MUTE_MEMBERS"))?;
                let n = ctx.name(&who);
                if b["on"] == json!(false) { if let Some(m) = sv["voiceMuted"].as_object_mut() { m.remove(&who); } audit(sv, user, "unmuted in calls", &n); } else { sv["voiceMuted"][&who] = json!(true); audit(sv, user, "muted in calls", &n); }
                Ok(json!(true))
            }
            "setNick" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i];
                let who = if s(b, "user").is_empty() { user.to_string() } else { s(b, "user").to_lowercase() };
                if who != user { target(sv, &who, Some("MANAGE_NICKNAMES"))?; } else if !p.can(sv, user, "CHANGE_NICKNAME") { return Err(format!("You may not change your nickname in {}.", s(sv, "name"))); }
                if !Self::in_server(sv, &who, ctx) { return Err("They are not in the server.".into()); }
                let n = clean_name(&b["nick"], 32); if !n.is_empty() { sv["nicks"][&who] = json!(n); } else if let Some(m) = sv["nicks"].as_object_mut() { m.remove(&who); }
                if who != user { let nm = ctx.name(&who); audit(sv, user, "changed the nickname of", &nm); }
                Ok(json!(true))
            }
            "setRoles" => {
                let i = need(db, &sid, "MANAGE_ROLES")?; let sv = &mut db["servers"][i]; let who = s(b, "user").to_lowercase();
                if !Self::in_server(sv, &who, ctx) { return Err("They are not in the server.".into()); }
                if who != user && !p.is_owner(sv, user) { target(sv, &who, None)?; }
                let roles = arr(&sv["roles"]);
                let mut want: Vec<String> = vec![]; for x in arr(&b["roles"]) { let id = match x { Value::String(t) => t, o => o.to_string() }; if roles.iter().any(|r| r["id"] == json!(id)) && !want.contains(&id) { want.push(id); } }
                let before = strs(&sv["memberRoles"][&who]);
                for id in want.iter().filter(|x| !before.contains(x)).chain(before.iter().filter(|x| !want.contains(x))) { if let Some(r) = roles.iter().find(|r| r["id"] == json!(id)) { if !p.role_below(sv, user, r) { return Err(format!("The role \"{}\" is not below your own.", s(r, "name"))); } } }
                if !want.is_empty() { sv["memberRoles"][&who] = json!(want); } else if let Some(m) = sv["memberRoles"].as_object_mut() { m.remove(&who); }
                let mid = member_role(sv);
                if !real_roles(sv, &who).is_empty() || mid.map(|m| want.contains(&m)).unwrap_or(false) { if let Some(m) = sv["temp"].as_object_mut() { m.remove(&who); } }
                let names = want.iter().filter_map(|id| roles.iter().find(|r| r["id"] == json!(id)).map(|r| s(r, "name"))).collect::<Vec<_>>().join(", ");
                let n = ctx.name(&who); audit(sv, user, "changed the roles of", &format!("{n}: {}", if names.is_empty() { "none".to_string() } else { names }));
                Ok(json!(true))
            }
            "selfRole" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i];
                let Some(r) = arr(&sv["roles"]).into_iter().find(|x| x["id"] == b["role"]).filter(|r| r["selfAssign"] == json!(true)) else { return Err("That role is not one you can pick yourself.".into()) };
                if p.timed_out(sv, user) { return Err("Not while you are timed out.".into()); }
                let rid = s(&r, "id"); let cur = strs(&sv["memberRoles"][user]);
                let next: Vec<String> = if b["on"] == json!(false) { cur.into_iter().filter(|x| *x != rid).collect() } else { let mut c = cur; if !c.contains(&rid) { c.push(rid); } c };
                if next.is_empty() { if let Some(m) = sv["memberRoles"].as_object_mut() { m.remove(user); } } else { sv["memberRoles"][user] = json!(next); }
                Ok(json!(true))
            }
            "createRole" => { let i = need(db, &sid, "MANAGE_ROLES")?; let sv = &mut db["servers"][i]; let r = new_role(sv, user, b, &p)?; audit(sv, user, "made the role", &s(&r, "name")); Ok(r) }
            "updateRole" => {
                let i = need(db, &sid, "MANAGE_ROLES")?; let sv = &mut db["servers"][i];
                let Some(ri) = arr(&sv["roles"]).iter().position(|x| x["id"] == b["role"]) else { return Err("No such role.".into()) };
                let r = sv["roles"][ri].clone();
                if !p.role_below(sv, user, &r) { return Err("That role is not below your own.".into()); }
                let mut r = r;
                if b.get("name").is_some() { let n = clean_name(&b["name"], 32); if !n.is_empty() { r["name"] = json!(n); } }
                if b.get("color").is_some() { r["color"] = json!(if color_ok(&s(b, "color")) { s(b, "color") } else { String::new() }); }
                if b.get("hoist").is_some() { r["hoist"] = json!(truthy(&b["hoist"])); }
                if b.get("mentionable").is_some() { r["mentionable"] = json!(truthy(&b["mentionable"])); }
                if b.get("selfAssign").is_some() {
                    if truthy(&b["selfAssign"]) && truthy(&r["builtin"]) { return Err("Built-in levels are handed out, not picked.".into()); }
                    if truthy(&b["selfAssign"]) { safe_for_self(&if b.get("perms").is_some() { perms::clean_perms(&b["perms"], &perms::all_perms()) } else { strs(&r["perms"]) })?; }
                    r["selfAssign"] = json!(truthy(&b["selfAssign"]));
                }
                if b.get("perms").is_some() && r["selfAssign"] == json!(true) { safe_for_self(&perms::clean_perms(&b["perms"], &perms::all_perms()))?; }
                if b.get("perms").is_some() && (r["builtin"] == json!("coowner") || r["builtin"] == json!("superadmin")) { return Err(format!("{} always has every permission.", s(&r, "name"))); }
                if b.get("position").is_some() && truthy(&r["builtin"]) { return Err("The built-in levels keep their order; move your own roles between them.".into()); }
                if b.get("perms").is_some() {
                    let ps = perms::clean_perms(&b["perms"], &perms::all_perms()); let cur = strs(&r["perms"]);
                    let diff: Vec<String> = ps.iter().filter(|x| !cur.contains(x)).cloned().chain(cur.iter().filter(|x| !ps.contains(x)).cloned()).collect();
                    if !p.grantable(sv, user, &diff) { return Err("You can only change permissions you have yourself.".into()); }
                    r["perms"] = json!(ps);
                }
                if b.get("position").is_some() {
                    let cur = r["position"].as_i64().unwrap_or(0);
                    let mut pos = (num(&b["position"]).unwrap_or(0) as i32 as i64).clamp(1, perms::COOWNER_POS - 1); let dir = if pos > cur { 1 } else { -1 };
                    while is_builtin_pos(pos) { pos += dir; }
                    if pos < 1 || pos >= perms::COOWNER_POS { return Err("A custom role stays between Member and Co-owner.".into()); }
                    if !(p.rank(sv, user) > pos as f64) { return Err("A role can only move below your own.".into()); }
                    r["position"] = json!(pos);
                }
                sv["roles"][ri] = r.clone(); audit(sv, user, "changed the role", &s(&r, "name")); Ok(r)
            }
            "deleteRole" => {
                let i = need(db, &sid, "MANAGE_ROLES")?; let sv = &mut db["servers"][i];
                let Some(ri) = arr(&sv["roles"]).iter().position(|x| x["id"] == b["role"]) else { return Err("No such role.".into()) };
                let r = sv["roles"][ri].clone();
                if truthy(&r["builtin"]) { return Err(format!("{} is a built-in level; it stays (rename or re-permission it instead).", s(&r, "name"))); }
                if !p.role_below(sv, user, &r) { return Err("That role is not below your own.".into()); }
                sv["roles"].as_array_mut().unwrap().remove(ri); let rid = s(&r, "id");
                if let Some(m) = sv["memberRoles"].as_object_mut() { for v in m.values_mut() { let l: Vec<String> = strs(v).into_iter().filter(|x| *x != rid).collect(); *v = json!(l); } m.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false)); }
                for ch in sv["channels"].as_array_mut().unwrap() { let ow: Vec<Value> = arr(&ch["overwrites"]).into_iter().filter(|o| !(o["type"] == json!("role") && o["id"] == json!(rid))).collect(); ch["overwrites"] = json!(ow); }
                audit(sv, user, "deleted the role", &s(&r, "name")); Ok(json!(true))
            }
            "createInvite" => {
                let i = need(db, &sid, "CREATE_INVITE")?; let sv = &mut db["servers"][i]; not_everyone(sv, "invite links")?;
                if truthy(&sv["settings"]["invitesPaused"]) { return Err(format!("Invites to {} are paused.", s(sv, "name"))); }
                if arr(&sv["invites"]).iter().filter(|iv| live(iv)).count() >= INVITE_MAX { return Err("That server has enough open invites; revoke some.".into()); }
                let age = num(&b["maxAge"]).filter(|a| INVITE_AGES.contains(a)).unwrap_or(1440);
                let max_uses = num(&b["maxUses"]).filter(|m| [0, 1, 5, 10, 25, 100].contains(m)).unwrap_or(0);
                let code = { let raw: String = vr_core::b64(&vr_core::crypto::random_bytes(6)).chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect(); format!("{raw}{}", hexid(2)) };
                let iv = json!({ "code": code, "by": user, "at": iso(), "expiresAt": if age > 0 { iso_at(now() as i64 + age * 60000) } else { String::new() }, "maxUses": max_uses, "uses": 0, "temporary": truthy(&b["temporary"]) });
                let mut list: Vec<Value> = arr(&sv["invites"]).into_iter().filter(live).collect(); list.push(iv.clone()); sv["invites"] = json!(list);
                let ages = if age > 0 { format!("expires in {}", if age >= 1440 { format!("{} d", fmt_num(age as f64 / 1440.0)) } else if age >= 60 { format!("{} h", fmt_num(age as f64 / 60.0)) } else { format!("{age} min") }) } else { "never expires".into() };
                audit(sv, user, "made an invite", &format!("{ages}{}{}", if max_uses > 0 { format!(", {max_uses} uses") } else { String::new() }, if iv["temporary"] == json!(true) { ", temporary" } else { "" }));
                Ok(pub_invite(&iv))
            }
            "revokeInvite" => {
                let Some((i, j)) = find_invite(db, &b["code"]) else { return Err("No such invite.".into()) };
                let sv = &mut db["servers"][i]; let iv = sv["invites"][j].clone();
                if !Self::in_server(sv, user, ctx) || !(iv["by"] == json!(user) || p.can(sv, user, "MANAGE_INVITES")) { return Err("Only who made the invite (or someone who manages invites) revokes it.".into()); }
                sv["invites"].as_array_mut().unwrap().remove(j); audit(sv, user, "revoked an invite", ""); Ok(json!(true))
            }
            "previewInvite" => {
                let f = find_invite(db, &b["code"]).filter(|(i, j)| live(&db["servers"][*i]["invites"][*j])); let Some((i, j)) = f else { return Err("That invite is invalid or has expired.".into()) };
                let sv = &db["servers"][i]; let iv = &sv["invites"][j];
                Ok(json!({ "server": sv["id"], "name": sv["name"], "icon": sv["icon"], "color": sv["color"], "description": sv["description"], "members": Self::server_members(sv, ctx).len(), "locked": truthy(&sv["password"]), "paused": truthy(&sv["settings"]["invitesPaused"]), "temporary": truthy(&iv["temporary"]), "by": iv["by"], "already": Self::in_server(sv, user, ctx), "banned": arr(&sv["bans"]).iter().any(|x| x["user"] == json!(user)) }))
            }
            "joinByCode" => {
                let f = find_invite(db, &b["code"]).filter(|(i, j)| live(&db["servers"][*i]["invites"][*j])); let Some((i, j)) = f else { return Err("That invite is invalid or has expired.".into()) };
                if Self::in_server(&db["servers"][i], user, ctx) { return Ok(db["servers"][i].clone()); }
                let sv = &mut db["servers"][i];
                if truthy(&sv["settings"]["invitesPaused"]) { return Err(format!("Invites to {} are paused right now.", s(sv, "name"))); }
                if arr(&sv["bans"]).iter().any(|x| x["user"] == json!(user)) { return Err(format!("You are banned from {}.", s(sv, "name"))); }
                self.check_password(sv, user, &b["password"])?;
                let uses = sv["invites"][j]["uses"].as_i64().unwrap_or(0) + 1; sv["invites"][j]["uses"] = json!(uses);
                let by = s(&sv["invites"][j], "by"); let temp = truthy(&sv["invites"][j]["temporary"]);
                join(sv, user, format!("with an invite from {}", ctx.name(&by)), temp, ann)?;
                if !live(&sv["invites"][j]) { sv["invites"].as_array_mut().unwrap().remove(j); }
                Ok(sv.clone())
            }
            "createChannel" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i];
                if !p.can(sv, user, "CREATE_CHANNELS") && !p.can(sv, user, "MANAGE_CHANNELS") { return Err(format!("You may not make channels in {}.", s(sv, "name"))); }
                let id = slug(&match &b["name"] { Value::String(t) => t.clone(), Value::Null => String::new(), o => o.to_string() }); if id.is_empty() { return Err("A channel needs a name (letters, digits, dashes).".into()); }
                if arr(&sv["channels"]).iter().any(|c| c["id"] == json!(id)) { return Err("That channel exists already.".into()); }
                if arr(&sv["channels"]).len() >= CHANNEL_MAX { return Err("That server has enough channels.".into()); }
                let pos = arr(&sv["channels"]).iter().map(|c| c["position"].as_f64().unwrap_or(0.0)).fold(0.0, f64::max) + 1.0;
                let mut ch = norm_channel(&json!({ "id": id, "name": id, "topic": clean_name(&b["topic"], 120), "category": clean_name(&b["category"], 32), "position": pos, "announce": truthy(&b["announce"]), "nsfw": truthy(&b["nsfw"]), "slowmode": num(&b["slowmode"]).unwrap_or(0), "createdBy": user }), 0, sv);
                ch["overwrites"] = json!(if b.get("overwrites").is_some() { clean_overwrites(sv, user, &b["overwrites"], &p, ctx)? } else { clean_overwrites(sv, user, &private_overwrites(&b["roles"]), &p, ctx)? });
                sv["channels"].as_array_mut().unwrap().push(ch.clone());
                if !p.can_in(sv, Some(&ch), user, "VIEW_CHANNEL") { sv["channels"].as_array_mut().unwrap().pop(); return Err("Those permissions would hide the channel from you; give one of your roles access too.".into()); }
                audit(sv, user, "made the channel", &format!("#{id}")); Ok(ch)
            }
            "updateChannel" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i];
                let Some(ci) = arr(&sv["channels"]).iter().position(|c| c["id"] == b["channel"]) else { return Err("No such channel.".into()) };
                let ch0 = sv["channels"][ci].clone();
                let manage = p.can_in(sv, Some(&ch0), user, "MANAGE_CHANNELS"); let own = ch0["createdBy"] == json!(user) && p.can(sv, user, "CREATE_CHANNELS");
                if !manage && !own { return Err("Only who made the channel (or someone who manages channels) changes it.".into()); }
                let mut ch = ch0.clone();
                if b.get("name").is_some() { let n = slug(&s(b, "name")); if !n.is_empty() { ch["name"] = json!(n); } }
                if b.get("topic").is_some() { ch["topic"] = json!(clean_name(&b["topic"], 120)); }
                if b.get("category").is_some() { ch["category"] = json!(clean_name(&b["category"], 32)); }
                if b.get("announce").is_some() { ch["announce"] = json!(truthy(&b["announce"])); }
                if b.get("nsfw").is_some() { ch["nsfw"] = json!(truthy(&b["nsfw"])); }
                if b.get("slowmode").is_some() { ch["slowmode"] = json!(num(&b["slowmode"]).filter(|x| SLOWMODES.contains(x)).unwrap_or(0)); }
                if b.get("overwrites").is_some() || b.get("roles").is_some() {
                    if sv["everyone"] == json!(true) && ch["id"] == json!("general") { return Err("#general is everyone's; its permissions stay.".into()); }
                    if !manage && !p.can_in(sv, Some(&ch0), user, "MANAGE_ROLES") && !own { return Err("You need \"manage channels\" or \"manage roles\" to change channel permissions.".into()); }
                    let next = if b.get("overwrites").is_some() { clean_overwrites(sv, user, &b["overwrites"], &p, ctx)? } else { clean_overwrites(sv, user, &private_overwrites(&b["roles"]), &p, ctx)? };
                    ch["overwrites"] = json!(next);
                    if !p.can_in(sv, Some(&ch), user, "VIEW_CHANNEL") { return Err("That would hide the channel from you; keep access for one of your roles or yourself.".into()); }
                }
                let changed = ch != ch0; let nm = s(&ch, "name");
                sv["channels"][ci] = ch.clone();
                if changed { audit(sv, user, "changed the channel", &format!("#{nm}")); }
                Ok(ch)
            }
            "moveChannel" => {
                let i = need(db, &sid, "MANAGE_CHANNELS")?; let sv = &mut db["servers"][i];
                let mut list = arr(&sv["channels"]); list.sort_by(|a, c| a["position"].as_f64().unwrap_or(0.0).partial_cmp(&c["position"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
                let Some(k) = list.iter().position(|c| c["id"] == b["channel"]) else { return Err("No such channel.".into()) };
                let j = (k as i64 + if s(b, "dir") == "up" { -1 } else { 1 }).clamp(0, list.len() as i64 - 1) as usize;
                if j != k { let c = list.remove(k); list.insert(j, c); for (n, c) in list.iter_mut().enumerate() { c["position"] = json!(n); } sv["channels"] = json!(list); }
                Ok(json!(true))
            }
            "deleteChannel" => {
                let i = member(db, &sid)?; let sv = &mut db["servers"][i];
                let Some(ci) = arr(&sv["channels"]).iter().position(|c| c["id"] == b["channel"]) else { return Err("No such channel.".into()) };
                let ch = sv["channels"][ci].clone();
                if ch["id"] == json!("general") { return Err("#general stays.".into()); }
                if !(p.can_in(sv, Some(&ch), user, "MANAGE_CHANNELS") || ch["createdBy"] == json!(user)) { return Err("Only who made the channel (or someone who manages channels) deletes it.".into()); }
                audit(sv, user, "deleted the channel", &format!("#{}", s(&ch, "name")));
                let conv = format!("ch:{}/{}", s(sv, "id"), s(&ch, "id"));
                for k in ["systemChannel", "rulesChannel"] { if sv["settings"][k] == ch["id"] { sv["settings"][k] = json!(""); } }
                sv["channels"].as_array_mut().unwrap().remove(ci);
                if let Some(m) = db["pins"].as_object_mut() { m.remove(&conv); }
                Ok(json!(true))
            }
            "pin" => {
                let conv = s(b, "conv"); let seq = num(&b["seq"]).unwrap_or(0);
                let Some(c) = parse(&conv) else { return Err("Unknown conversation.".into()) };
                let bad = match &c { Conv::Dm(a, bb) => !(a < bb && (a == user || bb == user)), Conv::Grp(_) | Conv::Ch(..) => !Self::members_in(db, &conv, ctx).map(|l| l.iter().any(|x| x == user)).unwrap_or(false), Conv::Room => false };
                if bad { return Err("Unknown conversation.".into()); }
                if matches!(c, Conv::Room | Conv::Ch(..)) { if let Some((sv, ch)) = Self::chan_of(db, &conv) { if !p.can_in(sv, Some(ch), user, "PIN_MESSAGES") { return Err("You need the \"pin messages\" permission here.".into()); } } }
                if (ctx.msg_conv)(seq).as_deref() != Some(conv.as_str()) { return Err("No such message in that conversation.".into()); }
                let mut list = arr(&db["pins"][&conv]);
                if b["on"] == json!(false) { list.retain(|x| x["seq"].as_i64() != Some(seq)); }
                else if !list.iter().any(|x| x["seq"].as_i64() == Some(seq)) { if list.len() >= PIN_MAX { return Err(format!("Up to {PIN_MAX} pins per conversation; unpin one first.")); } list.push(json!({ "seq": seq, "by": user, "at": iso() })); }
                db["pins"][&conv] = json!(list);
                if let Some((sv, ch)) = Self::chan_of(db, &conv).map(|(a, b)| (s(a, "id"), s(b, "name"))) { if let Some(i) = find(db, &sv) { if db["servers"][i]["everyone"] != json!(true) { audit(&mut db["servers"][i], user, if b["on"] == json!(false) { "unpinned a message in" } else { "pinned a message in" }, &format!("#{ch}")); } } }
                Ok(json!({ "pins": db["pins"][&conv] }))
            }
            "createGroup" => {
                let mut members = vec![user.to_string()]; for u in user_list(&b["members"]) { if !members.contains(&u) { members.push(u); } }
                if members.len() < 2 { return Err("A group needs at least one other person.".into()); }
                if members.len() > GROUP_MAX { return Err(format!("A group holds up to {GROUP_MAX} people.")); }
                if arr(&db["groups"]).iter().filter(|g| g["owner"] == json!(user)).count() >= GROUPS_PER_USER { return Err(format!("You own {GROUPS_PER_USER} groups already; leave or hand one on first.")); }
                let nm = clean_name(&b["name"], 40);
                let nm = if nm.is_empty() { members.iter().map(|u| ctx.name(u)).collect::<Vec<_>>().join(", ").chars().take(40).collect() } else { nm };
                let g = json!({ "id": hexid(5), "name": nm, "members": members, "owner": user, "createdBy": user, "createdAt": iso() });
                db["groups"].as_array_mut().unwrap().push(g.clone()); Ok(g)
            }
            "renameGroup" | "addToGroup" | "removeFromGroup" | "transferGroup" | "leaveGroup" => {
                let gi = db["groups"].as_array().unwrap().iter().position(|g| g["id"] == b["group"] && strs(&g["members"]).iter().any(|x| x == user)).ok_or("No such group (or you are not in it).")?;
                let g = &mut db["groups"][gi];
                match name {
                    "renameGroup" => { let n = clean_name(&b["name"], 40); if !n.is_empty() { g["name"] = json!(n); } Ok(g.clone()) }
                    "addToGroup" => { let mut next = vec![user.to_string()]; for u in user_list(&json!(strs(&g["members"]).into_iter().map(Value::String).chain(arr(&as_list(&b["add"]))).collect::<Vec<_>>())) { if !next.contains(&u) { next.push(u); } } if next.len() > GROUP_MAX { return Err(format!("A group holds up to {GROUP_MAX} people.")); } g["members"] = json!(next); Ok(g.clone()) }
                    "removeFromGroup" => { let who = s(b, "user").to_lowercase(); if g["owner"] != json!(user) { return Err("Only the group's owner removes people.".into()); } if who == user { return Err("Leave the group instead.".into()); } let l: Vec<String> = strs(&g["members"]).into_iter().filter(|x| *x != who).collect(); g["members"] = json!(l); Ok(g.clone()) }
                    "transferGroup" => { let to = s(b, "user").to_lowercase(); if g["owner"] != json!(user) { return Err("Only the group's owner hands it on.".into()); } if !strs(&g["members"]).contains(&to) || to == user { return Err("Hand it to another member.".into()); } g["owner"] = json!(to); Ok(g.clone()) }
                    _ => {
                        let l: Vec<String> = strs(&g["members"]).into_iter().filter(|x| x != user).collect(); g["members"] = json!(l.clone());
                        if l.is_empty() { let id = s(g, "id"); db["groups"].as_array_mut().unwrap().remove(gi); if let Some(m) = db["pins"].as_object_mut() { m.remove(&format!("grp:{id}")); } } else if g["owner"] == json!(user) { g["owner"] = json!(l[0]); }
                        Ok(json!(true))
                    }
                }
            }
            _ => Err("Unknown endpoint".into()),
        }
    }
    fn check_password(&self, sv: &Value, user: &str, pw: &Value) -> R<()> {
        if !sv["password"].is_object() { return Ok(()); }
        let k = format!("{user}|{}", s(sv, "id")); let now = now();
        let mut tries = self.pw_tries.lock().unwrap();
        let list: Vec<u64> = tries.get(&k).cloned().unwrap_or_default().into_iter().filter(|t| now - t < PW_WINDOW).collect();
        if list.len() >= PW_TRIES { return Err(format!("Too many wrong passwords for {}. Try again in a few minutes.", s(sv, "name"))); }
        let pw = match pw { Value::String(t) => t.clone(), Value::Null => String::new(), o => o.to_string() };
        let salt = vr_core::unb64(&s(&sv["password"], "salt")).unwrap_or_default();
        let want = vr_core::unb64(&s(&sv["password"], "hash")).unwrap_or_default();
        let got = scrypt32(&pw, &salt);
        use subtle::ConstantTimeEq;
        if want.len() != got.len() || !bool::from(got.ct_eq(&want[..])) { let mut l = list; l.push(now); tries.insert(k, l); return Err(if pw.is_empty() { format!("{} is password locked: enter its password.", s(sv, "name")) } else { "Wrong server password.".into() }); }
        tries.remove(&k); Ok(())
    }
    /// An account was deleted: out of every server and group.
    pub fn drop_user(&self, user: &str, ctx: &Ctx) {
        let mut db = self.db.lock().unwrap();
        for sv in db["servers"].as_array_mut().unwrap() {
            if sv["everyone"] == json!(true) { for k in ["memberRoles", "nicks", "timeouts", "voiceMuted", "joined"] { if let Some(m) = sv[k].as_object_mut() { m.remove(user); } } continue; }
            remove_from(sv, user); if let Some(m) = sv["timeouts"].as_object_mut() { m.remove(user); }
            if sv["owner"] == json!(user) { sv["owner"] = json!(strs(&sv["members"]).into_iter().find(|u| ctx.has(u) && u != user).unwrap_or_default()); }
        }
        for g in db["groups"].as_array_mut().unwrap() { let l: Vec<String> = strs(&g["members"]).into_iter().filter(|x| x != user).collect(); g["members"] = json!(l.clone()); if g["owner"] == json!(user) { g["owner"] = json!(l.first().cloned().unwrap_or_default()); } }
        db["servers"].as_array_mut().unwrap().retain(|sv| sv["everyone"] == json!(true) || !arr(&sv["members"]).is_empty());
        db["groups"].as_array_mut().unwrap().retain(|g| !arr(&g["members"]).is_empty());
        self.save(&db);
    }
    /// Every 20 s: temporary members who went offline leave; ended timeouts go. True when something changed.
    pub fn sweep(&self, online: &dyn Fn(&str) -> bool) -> bool {
        let mut db = self.db.lock().unwrap(); let mut any = false; let now_ms = now();
        let mut empty = vec![];
        for (i, sv) in db["servers"].as_array_mut().unwrap().iter_mut().enumerate() {
            for (u, at) in sv["temp"].as_object().cloned().unwrap_or_default() {
                if now_ms.saturating_sub(at.as_u64().unwrap_or(0)) < TEMP_GRACE || online(&u) { continue; }
                if !real_roles(sv, &u).is_empty() { sv["temp"].as_object_mut().unwrap().remove(&u); any = true; continue; }
                remove_from(sv, &u); audit(sv, &u, "left", "temporary membership ended (went offline)"); any = true;
                if sv["everyone"] != json!(true) && arr(&sv["members"]).is_empty() && !empty.contains(&i) { empty.push(i); }
            }
            let ended: Vec<String> = sv["timeouts"].as_object().map(|m| m.iter().filter(|(_, t)| ms_of(t).map(|x| x <= now_ms as i64).unwrap_or(true)).map(|(u, _)| u.clone()).collect()).unwrap_or_default();
            for u in ended { sv["timeouts"].as_object_mut().unwrap().remove(&u); any = true; }
        }
        for i in empty.into_iter().rev() { db["servers"].as_array_mut().unwrap().remove(i); }
        if any { self.save(&db); }
        any
    }
}
fn fmt_num(x: f64) -> String { if x.fract() == 0.0 { format!("{}", x as i64) } else { format!("{x}") } }
fn local_time(ms: i64) -> String { chrono::DateTime::from_timestamp_millis(ms).map(|d| d.with_timezone(&chrono::Local).format("%-m/%-d/%Y, %-I:%M:%S %p").to_string()).unwrap_or_default() }
fn scrypt32(pw: &str, salt: &[u8]) -> Vec<u8> { let mut out = vec![0u8; 32]; let params = scrypt::Params::new(14, 8, 1, 32).expect("params"); let _ = scrypt::scrypt(pw.as_bytes(), salt, &params, &mut out); out }
fn live(iv: &Value) -> bool { let exp = s(iv, "expiresAt"); (exp.is_empty() || ms_of(&iv["expiresAt"]).map(|t| t > now() as i64).unwrap_or(false)) && (iv["maxUses"].as_i64().unwrap_or(0) == 0 || iv["uses"].as_i64().unwrap_or(0) < iv["maxUses"].as_i64().unwrap_or(0)) }
fn pub_invite(iv: &Value) -> Value { json!({ "code": iv["code"], "by": iv["by"], "at": iv["at"], "expiresAt": s(iv, "expiresAt"), "maxUses": iv["maxUses"].as_i64().unwrap_or(0), "uses": iv["uses"], "temporary": truthy(&iv["temporary"]) }) }
fn audit(sv: &mut Value, by: &str, what: &str, detail: &str) {
    if !sv["audit"].is_array() { sv["audit"] = json!([]); }
    let mut e = json!({ "at": iso(), "by": by, "what": what }); if !detail.is_empty() { e["detail"] = json!(detail.chars().take(200).collect::<String>()); }
    let a = sv["audit"].as_array_mut().unwrap(); a.push(e); if a.len() > AUDIT_KEEP { let k = a.len() - AUDIT_KEEP; a.drain(0..k); }
}
fn member_role(sv: &Value) -> Option<String> { arr(&sv["roles"]).iter().find(|r| r["builtin"] == json!("member")).map(|r| s(r, "id")) }
fn real_roles(sv: &Value, u: &str) -> Vec<String> { let roles = arr(&sv["roles"]); strs(&sv["memberRoles"][u]).into_iter().filter(|id| roles.iter().find(|r| r["id"] == json!(id)).map(|r| r["builtin"] != json!("member")).unwrap_or(false)).collect() }
fn remove_from(sv: &mut Value, who: &str) {
    let l: Vec<String> = strs(&sv["members"]).into_iter().filter(|x| x != who).collect(); sv["members"] = json!(l);
    for k in ["memberRoles", "nicks", "temp", "pending", "joined", "voiceMuted"] { if let Some(m) = sv[k].as_object_mut() { m.remove(who); } }
}
fn maybe_drop(db: &mut Value, i: usize) { if db["servers"][i]["everyone"] != json!(true) && arr(&db["servers"][i]["members"]).is_empty() { db["servers"].as_array_mut().unwrap().remove(i); } }
fn is_builtin_pos(n: i64) -> bool { perms::builtins().iter().any(|b| b.position == n) }
fn safe_for_self(ps: &[String]) -> R<()> { let bad: Vec<String> = ps.iter().filter(|p| !SELF_SAFE.contains(&p.as_str())).map(|p| perm_name(p)).collect(); if bad.is_empty() { Ok(()) } else { Err(format!("A self-assignable role may only carry everyday permissions (not: {}).", bad.join(", "))) } }
fn new_role(sv: &mut Value, user: &str, b: &Value, p: &Perms) -> R<Value> {
    if arr(&sv["roles"]).len() >= ROLE_MAX { return Err("That server has enough roles.".into()); }
    let tpls = perms::templates(); let tpl = b["template"].as_str().and_then(|t| tpls.get(t)).cloned();
    let name = clean_name(if truthy(&b["name"]) { &b["name"] } else { tpl.as_ref().map(|t| &t["name"]).unwrap_or(&Value::Null) }, 32); if name.is_empty() { return Err("A role needs a name.".into()); }
    let ps = perms::clean_perms(if b.get("perms").is_some() { &b["perms"] } else { tpl.as_ref().map(|t| &t["perms"]).unwrap_or(&Value::Null) }, &perms::all_perms());
    if !p.grantable(sv, user, &ps) { return Err("You can only give a role permissions you have yourself.".into()); }
    let my_rank = p.rank(sv, user);
    let mut position: i64 = if my_rank.is_infinite() { 11 } else { ((my_rank - 1.0) as i64).min(11) };
    while position > 0 && is_builtin_pos(position) { position -= 1; }
    if !(position >= 1) || !(my_rank > position as f64) { return Err("You can only make roles below your own.".into()); }
    let color = { let c = if truthy(&b["color"]) { s(b, "color") } else { tpl.as_ref().map(|t| s(t, "color")).unwrap_or_default() }; if color_ok(&c) { c } else { String::new() } };
    let mut r = json!({ "id": hexid(4), "name": name, "color": color, "perms": ps, "position": position, "hoist": b["hoist"] != json!(false), "mentionable": truthy(&b["mentionable"]), "selfAssign": false });
    if truthy(&b["selfAssign"]) { safe_for_self(&ps)?; r["selfAssign"] = json!(true); }
    sv["roles"].as_array_mut().unwrap().push(r.clone());
    Ok(r)
}
fn clean_overwrites(sv: &Value, user: &str, list: &Value, p: &Perms, ctx: &Ctx) -> R<Vec<Value>> {
    let mut out = vec![];
    for o in arr(list) {
        let ty = if o["type"] == json!("member") { "member" } else { "role" }; let id = match &o["id"] { Value::String(t) => t.to_lowercase(), Value::Null => String::new(), v => v.to_string().to_lowercase() };
        if ty == "role" && id != "everyone" && !arr(&sv["roles"]).iter().any(|r| r["id"] == json!(id)) { continue; }
        if ty == "member" && !Guilds::in_server(sv, &id, ctx) { continue; }
        let allow = perms::clean_perms(&o["allow"], perms::CHANNEL_PERMS); let deny: Vec<String> = perms::clean_perms(&o["deny"], perms::CHANNEL_PERMS).into_iter().filter(|x| !allow.contains(x)).collect();
        let both: Vec<String> = allow.iter().chain(deny.iter()).cloned().collect();
        if !p.grantable(sv, user, &both) { return Err("You can only allow or deny permissions you have yourself.".into()); }
        if !allow.is_empty() || !deny.is_empty() { out.push(json!({ "type": ty, "id": id, "allow": allow, "deny": deny })); }
    }
    Ok(out)
}
fn private_overwrites(roles: &Value) -> Value {
    let mut ids: Vec<String> = vec![]; for x in arr(roles) { let id = match x { Value::String(t) => t, o => o.to_string() }; if !ids.contains(&id) { ids.push(id); } }
    if ids.is_empty() { return json!([]); }
    let mut v = vec![json!({ "type": "role", "id": "everyone", "allow": [], "deny": ["VIEW_CHANNEL"] })];
    v.extend(ids.into_iter().map(|id| json!({ "type": "role", "id": id, "allow": ["VIEW_CHANNEL"], "deny": [] })));
    json!(v)
}
fn clean_settings(sv: &mut Value, b: &Value) {
    let chans: Vec<String> = arr(&sv["channels"]).iter().map(|c| s(c, "id")).collect();
    let st = &mut sv["settings"];
    if b.get("systemChannel").is_some() { let v = s(b, "systemChannel"); if v.is_empty() || chans.contains(&v) { st["systemChannel"] = json!(v); } }
    if b.get("rulesChannel").is_some() { let v = s(b, "rulesChannel"); st["rulesChannel"] = json!(if chans.contains(&v) { v } else { String::new() }); }
    if b.get("systemJoins").is_some() { st["systemJoins"] = json!(truthy(&b["systemJoins"])); }
    if b.get("systemLeaves").is_some() { st["systemLeaves"] = json!(truthy(&b["systemLeaves"])); }
    if b.get("notify").is_some() { st["notify"] = json!(if b["notify"] == json!("mentions") { "mentions" } else { "all" }); }
    if b.get("invitesPaused").is_some() { st["invitesPaused"] = json!(truthy(&b["invitesPaused"])); }
    if b.get("wait").is_some() { st["wait"] = json!(num(&b["wait"]).filter(|w| WAITS.contains(w)).unwrap_or(0)); }
}
fn norm_channel(ch: &Value, i: usize, sv: &Value) -> Value {
    let mut c = json!({ "id": ch["id"], "name": if truthy(&ch["name"]) { ch["name"].clone() } else { ch["id"].clone() }, "topic": s(ch, "topic"), "category": s(ch, "category"), "position": if ch["position"].is_number() { ch["position"].clone() } else { json!(i) },
        "announce": truthy(&ch["announce"]), "nsfw": truthy(&ch["nsfw"]), "slowmode": ch["slowmode"].as_i64().filter(|x| SLOWMODES.contains(x)).unwrap_or(0), "createdBy": s(ch, "createdBy"), "overwrites": if ch["overwrites"].is_array() { ch["overwrites"].clone() } else { json!([]) } });
    let roles = strs(&ch["roles"]);
    if !roles.is_empty() && arr(&c["overwrites"]).is_empty() {
        let mut v = vec![json!({ "type": "role", "id": "everyone", "allow": [], "deny": ["VIEW_CHANNEL"] })];
        for id in roles { if arr(&sv["roles"]).iter().any(|r| r["id"] == json!(id)) { v.push(json!({ "type": "role", "id": id, "allow": ["VIEW_CHANNEL"], "deny": [] })); } }
        c["overwrites"] = json!(v);
    }
    let ow: Vec<Value> = arr(&c["overwrites"]).iter().map(|o| json!({ "type": if o["type"] == json!("member") { "member" } else { "role" }, "id": match &o["id"] { Value::String(t) => t.clone(), v => v.to_string() }, "allow": perms::clean_perms(&o["allow"], perms::CHANNEL_PERMS), "deny": perms::clean_perms(&o["deny"], perms::CHANNEL_PERMS) })).collect();
    c["overwrites"] = json!(ow);
    c
}
/// Every field a server may lack, filled in (older records upgraded), and the built-in role ladder.
fn norm_server(sv: &mut Value) {
    if !truthy(&sv["name"]) { sv["name"] = sv["id"].clone(); }
    if !truthy(&sv["icon"]) { sv["icon"] = json!(""); }
    sv["color"] = json!(if color_ok(&s(sv, "color")) { s(sv, "color") } else { String::new() });
    if !truthy(&sv["description"]) { sv["description"] = json!(""); }
    if !truthy(&sv["owner"]) { sv["owner"] = json!(s(sv, "createdBy")); }
    if !sv["members"].is_array() { sv["members"] = json!([]); }
    let roles: Vec<Value> = arr(&sv["roles"]).into_iter().map(|mut r| { r["perms"] = json!(perms::clean_perms(&r["perms"], &perms::all_perms())); r }).collect(); sv["roles"] = json!(roles);
    sv["memberRoles"] = obj(&sv["memberRoles"]);
    let ev: Vec<String> = if sv["everyonePerms"].is_array() { perms::clean_perms(&sv["everyonePerms"], &perms::all_perms()) } else { perms::DEFAULT_EVERYONE.iter().map(|x| x.to_string()).collect() };
    sv["everyonePerms"] = json!(if sv["permsVersion"] == json!(2) { ev } else { let mut u = ev; for d in perms::DEFAULT_EVERYONE { if !u.iter().any(|x| x == d) { u.push(d.to_string()); } } u.into_iter().filter(|x| x != "ADMINISTRATOR").collect() });
    for k in ["nicks", "pending", "temp", "timeouts", "voiceMuted", "joined"] { sv[k] = obj(&sv[k]); }
    for k in ["bans", "invites", "audit"] { if !sv[k].is_array() { sv[k] = json!([]); } }
    if !truthy(&sv["password"]) { sv["password"] = Value::Null; }
    let mut st = json!({ "systemChannel": "", "systemJoins": true, "systemLeaves": false, "rulesChannel": "", "notify": "all", "invitesPaused": false, "wait": 0 });
    for (k, v) in obj(&sv["settings"]).as_object().cloned().unwrap_or_default() { st[k] = v; }
    sv["settings"] = st;
    let snap = sv.clone();
    let chans: Vec<Value> = arr(&sv["channels"]).iter().enumerate().map(|(i, c)| norm_channel(c, i, &snap)).collect(); sv["channels"] = json!(chans);
    if !arr(&sv["channels"]).iter().any(|c| c["id"] == json!("general")) { let g = norm_channel(&json!({ "id": "general", "name": "general", "position": -1 }), 0, &snap); sv["channels"].as_array_mut().unwrap().insert(0, g); }
    sv["permsVersion"] = json!(2);
    ensure_builtins(sv);
}
fn ensure_builtins(sv: &mut Value) {
    let fresh = sv["rolesVersion"] != json!(3);
    for b in perms::builtins() {
        let roles = arr(&sv["roles"]);
        let found = roles.iter().position(|x| x["builtin"] == json!(b.key)).or_else(|| if fresh { roles.iter().position(|x| !truthy(&x["builtin"]) && s(x, "name").to_lowercase() == b.name.to_lowercase()) } else { None });
        let i = match found { Some(i) => i, None => { sv["roles"].as_array_mut().unwrap().push(json!({ "id": format!("b-{}", b.key), "name": b.name, "color": b.color, "perms": b.perms, "hoist": true, "mentionable": false })); arr(&sv["roles"]).len() - 1 } };
        let r = &mut sv["roles"][i]; r["builtin"] = json!(b.key); r["position"] = json!(b.position);
        if b.key == "coowner" || b.key == "superadmin" { r["perms"] = json!(["ADMINISTRATOR"]); }
    }
    if fresh {
        let mut custom: Vec<usize> = arr(&sv["roles"]).iter().enumerate().filter(|(_, r)| !truthy(&r["builtin"])).map(|(i, _)| i).collect();
        custom.sort_by(|a, b| sv["roles"][*a]["position"].as_f64().unwrap_or(0.0).partial_cmp(&sv["roles"][*b]["position"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
        for (k, i) in custom.into_iter().enumerate() { sv["roles"][i]["position"] = json!((41 + k as i64).min(49)); }
        let mem = member_role(sv).unwrap_or_default();
        if sv["everyone"] != json!(true) { for u in strs(&sv["members"]) { if sv["temp"].get(&u).map(truthy).unwrap_or(false) { continue; } let l = strs(&sv["memberRoles"][&u]); if !l.contains(&mem) { let mut n = vec![mem.clone()]; n.extend(l); sv["memberRoles"][&u] = json!(n); } } }
        sv["rolesVersion"] = json!(3);
    }
}
