//! Server permissions (moved from bridge/guild-perms.js), Discord's model and order:
//! server permissions → @everyone deny → @everyone allow → role denies (all roles held) → role allows → member deny →
//! member allow. Without VIEW_CHANNEL nothing else in that channel; without SEND_MESSAGES no voice messages; without
//! CONNECT no SPEAK / STREAM; an announcement channel needs ANNOUNCE (or MANAGE_MESSAGES) to post; a member in timeout
//! keeps only VIEW_CHANNEL + READ_MESSAGE_HISTORY; a server-muted member loses SPEAK. The owner (and ADMINISTRATOR) has
//! everything; in the everyone-server the hub's admin accounts count as owners. Ranking: people and roles strictly below.
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const CATEGORIES: &[(&str, &[(&str, &str)])] = &[
    ("General", &[("VIEW_CHANNEL", "See channels and read them (deny it on a channel to make it private)"), ("MANAGE_CHANNELS", "Make, change, reorder and delete any channel, and set channel permissions"), ("CREATE_CHANNELS", "Make channels, and change or delete the ones they made (a VibeRoller extra)"), ("MANAGE_ROLES", "Make, change and hand out roles below their own"), ("VIEW_AUDIT_LOG", "Read the audit log"), ("MANAGE_SERVER", "Change the server: name, icon, colour, description, password lock, welcome channel, other settings")]),
    ("Membership", &[("CREATE_INVITE", "Make invite links and invite people by name"), ("MANAGE_INVITES", "See and revoke everyone's invites (a VibeRoller extra)"), ("CHANGE_NICKNAME", "Change their own server nickname"), ("MANAGE_NICKNAMES", "Change other people's server nicknames"), ("KICK_MEMBERS", "Remove people from the server"), ("BAN_MEMBERS", "Ban and unban people (optionally deleting their recent messages)"), ("MODERATE_MEMBERS", "Time people out: they can read but not post, type or join calls for a while")]),
    ("Text", &[("SEND_MESSAGES", "Post messages"), ("SEND_VOICE_MESSAGES", "Send voice clips"), ("READ_MESSAGE_HISTORY", "Read messages sent before they joined (without it: only what arrives while they are a member)"), ("MANAGE_MESSAGES", "Delete other people's messages"), ("PIN_MESSAGES", "Pin and unpin messages"), ("ANNOUNCE", "Post in announcement channels (a VibeRoller extra)"), ("BYPASS_SLOWMODE", "Post without waiting for a channel's slowmode")]),
    ("Voice", &[("CONNECT", "Join calls"), ("SPEAK", "Talk in calls"), ("STREAM", "Turn on the camera or share the screen in calls"), ("MUTE_MEMBERS", "Mute other people in calls (server mute)")]),
    ("Advanced", &[("ADMINISTRATOR", "Every permission, every channel, skips channel permissions. Give with care.")]),
];
pub fn perm_ids() -> Vec<&'static str> { CATEGORIES.iter().flat_map(|(_, p)| p.iter().map(|x| x.0)).collect() }
pub const CHANNEL_PERMS: &[&str] = &["VIEW_CHANNEL", "MANAGE_CHANNELS", "MANAGE_ROLES", "CREATE_INVITE", "SEND_MESSAGES", "SEND_VOICE_MESSAGES", "READ_MESSAGE_HISTORY", "MANAGE_MESSAGES", "PIN_MESSAGES", "ANNOUNCE", "BYPASS_SLOWMODE", "CONNECT", "SPEAK", "STREAM", "MUTE_MEMBERS"];
pub const DEFAULT_EVERYONE: &[&str] = &["VIEW_CHANNEL", "CREATE_INVITE", "CHANGE_NICKNAME", "CREATE_CHANNELS", "SEND_MESSAGES", "SEND_VOICE_MESSAGES", "READ_MESSAGE_HISTORY", "CONNECT", "SPEAK", "STREAM"];
const MEMBER: &[&str] = &["VIEW_CHANNEL", "CHANGE_NICKNAME", "CREATE_INVITE", "SEND_MESSAGES", "SEND_VOICE_MESSAGES", "READ_MESSAGE_HISTORY", "CONNECT", "SPEAK", "STREAM"];
const MODERATOR_ADD: &[&str] = &["MANAGE_MESSAGES", "PIN_MESSAGES", "MODERATE_MEMBERS", "MUTE_MEMBERS", "KICK_MEMBERS", "MANAGE_NICKNAMES", "BYPASS_SLOWMODE", "VIEW_AUDIT_LOG"];
const SUPERMOD_ADD: &[&str] = &["BAN_MEMBERS", "MANAGE_INVITES", "ANNOUNCE", "CREATE_CHANNELS"];
const ADMIN_ADD: &[&str] = &["MANAGE_CHANNELS", "MANAGE_ROLES", "MANAGE_SERVER"];
pub struct Builtin { pub key: &'static str, pub name: &'static str, pub color: &'static str, pub position: i64, pub perms: Vec<&'static str> }
pub fn builtins() -> Vec<Builtin> {
    let m: Vec<&str> = MEMBER.to_vec();
    let md: Vec<&str> = [MEMBER, MODERATOR_ADD].concat();
    let sm: Vec<&str> = [MEMBER, MODERATOR_ADD, SUPERMOD_ADD].concat();
    let ad: Vec<&str> = [MEMBER, MODERATOR_ADD, SUPERMOD_ADD, ADMIN_ADD].concat();
    vec![
        Builtin { key: "member", name: "Member", color: "#2ecc71", position: 10, perms: m },
        Builtin { key: "moderator", name: "Moderator", color: "#3498db", position: 20, perms: md },
        Builtin { key: "supermod", name: "Super Moderator", color: "#1abc9c", position: 30, perms: sm },
        Builtin { key: "admin", name: "Admin", color: "#e67e22", position: 40, perms: ad },
        Builtin { key: "superadmin", name: "Super Admin", color: "#e74c3c", position: 50, perms: vec!["ADMINISTRATOR"] },
        Builtin { key: "coowner", name: "Co-owner", color: "#f1c40f", position: 60, perms: vec!["ADMINISTRATOR"] },
    ]
}
pub const COOWNER_POS: i64 = 60;
pub fn templates() -> Value { Value::Object(builtins().into_iter().map(|b| (b.key.to_string(), json!({ "name": b.name, "color": b.color, "perms": b.perms }))).collect()) }
pub fn perms_json() -> Value { Value::Object(CATEGORIES.iter().flat_map(|(_, p)| p.iter().map(|(k, d)| (k.to_string(), json!(d)))).collect()) }
pub fn categories_json() -> Value { json!(CATEGORIES.iter().map(|(n, p)| json!([n, p.iter().map(|x| x.0).collect::<Vec<_>>()])).collect::<Vec<_>>()) }

/// A permission list cleaned: known names only (older names renamed), each once, in the order given.
pub fn clean_perms(v: &Value, allowed: &[&str]) -> Vec<String> {
    let raw: Vec<String> = match v { Value::Array(a) => a.iter().map(|x| match x { Value::String(s) => s.clone(), Value::Null => "null".into(), o => o.to_string() }).collect(), Value::String(s) => s.split(|c: char| c == ',' || c.is_whitespace()).map(String::from).collect(), Value::Null => vec![], o => vec![o.to_string()] };
    let mut out: Vec<String> = vec![];
    for p in raw { let p = p.trim().to_uppercase(); let p = match p.as_str() { "INVITE" => "CREATE_INVITE".into(), "KICK" => "KICK_MEMBERS".into(), "BAN" => "BAN_MEMBERS".into(), "ADMIN" => "ADMINISTRATOR".into(), _ => p }; if allowed.contains(&p.as_str()) && !out.contains(&p) { out.push(p); } }
    out
}
pub fn all_perms() -> Vec<&'static str> { perm_ids() }

fn strs(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }
fn now_ms() -> i64 { vr_core::now_ms() as i64 }
fn ms_of(t: &Value) -> Option<i64> { t.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis()) }

/// The rules, given who the hub's admins are.
pub struct Perms<'a> { pub hub_admin: &'a dyn Fn(&str) -> bool }
impl Perms<'_> {
    pub fn is_owner(&self, s: &Value, user: &str) -> bool { if s["everyone"] == json!(true) { (!s["owner"].as_str().unwrap_or("").is_empty() && s["owner"] == json!(user)) || (self.hub_admin)(user) } else { s["owner"] == json!(user) } }
    pub fn roles_of<'v>(&self, s: &'v Value, user: &str) -> Vec<&'v Value> {
        let roles = s["roles"].as_array().map(|a| a.iter().collect::<Vec<_>>()).unwrap_or_default();
        let mut list: Vec<&Value> = strs(&s["memberRoles"][user]).iter().filter_map(|id| roles.iter().find(|r| r["id"] == json!(id)).copied()).collect();
        if s["everyone"] == json!(true) { if let Some(m) = roles.iter().find(|r| r["builtin"] == json!("member")) { if !list.iter().any(|x| std::ptr::eq(*x, *m)) { list.push(m); } } }
        list
    }
    pub fn timed_out(&self, s: &Value, user: &str) -> bool { ms_of(&s["timeouts"][user]).map(|t| t > now_ms()).unwrap_or(false) }
    pub fn is_admin(&self, s: &Value, user: &str) -> bool { self.is_owner(s, user) || self.roles_of(s, user).iter().any(|r| strs(&r["perms"]).iter().any(|p| p == "ADMINISTRATOR")) || strs(&s["everyonePerms"]).iter().any(|p| p == "ADMINISTRATOR") }
    fn base(&self, s: &Value, user: &str) -> Vec<String> {
        if self.is_admin(s, user) { return perm_ids().into_iter().map(String::from).collect(); }
        let mut set: Vec<String> = vec![]; let mut add = |p: String| { if !set.contains(&p) { set.push(p); } };
        for p in strs(&s["everyonePerms"]) { add(p); }
        for r in self.roles_of(s, user) { for p in strs(&r["perms"]) { add(p); } }
        set
    }
    fn limit(&self, s: &Value, user: &str, mut set: Vec<String>) -> Vec<String> {
        if self.is_admin(s, user) { return set; }
        if self.timed_out(s, user) { return set.into_iter().filter(|p| p == "VIEW_CHANNEL" || p == "READ_MESSAGE_HISTORY").collect(); }
        if s["voiceMuted"][user] == json!(true) || (!s["voiceMuted"][user].is_null() && s["voiceMuted"][user] != json!(false)) { set.retain(|p| p != "SPEAK"); }
        set
    }
    pub fn perms_of(&self, s: &Value, user: &str) -> Vec<String> { self.limit(s, user, self.base(s, user)) }
    pub fn perms_in(&self, s: &Value, ch: Option<&Value>, user: &str) -> Vec<String> {
        let Some(ch) = ch else { return self.perms_of(s, user) };
        if self.is_admin(s, user) { return perm_ids().into_iter().map(String::from).collect(); }
        let mut set = self.base(s, user);
        let ow: Vec<Value> = ch["overwrites"].as_array().cloned().unwrap_or_default();
        let apply = |set: &mut Vec<String>, o: &Value| { for p in strs(&o["deny"]) { set.retain(|x| *x != p); } for p in strs(&o["allow"]) { if !set.contains(&p) { set.push(p); } } };
        if let Some(o) = ow.iter().find(|o| o["type"] == json!("role") && o["id"] == json!("everyone")) { apply(&mut set, o); }
        let mine: BTreeSet<String> = strs(&s["memberRoles"][user]).into_iter().collect();
        let role_ow: Vec<&Value> = ow.iter().filter(|o| o["type"] == json!("role") && o["id"].as_str().map(|i| mine.contains(i)).unwrap_or(false)).collect();
        for o in &role_ow { for p in strs(&o["deny"]) { set.retain(|x| *x != p); } }
        for o in &role_ow { for p in strs(&o["allow"]) { if !set.contains(&p) { set.push(p); } } }
        if let Some(o) = ow.iter().find(|o| o["type"] == json!("member") && o["id"] == json!(user)) { apply(&mut set, o); }
        let has = |set: &Vec<String>, p: &str| set.iter().any(|x| x == p);
        if !has(&set, "VIEW_CHANNEL") { return vec![]; }
        if !has(&set, "SEND_MESSAGES") { set.retain(|x| x != "SEND_VOICE_MESSAGES"); }
        if ch["announce"] == json!(true) && !has(&set, "ANNOUNCE") && !has(&set, "MANAGE_MESSAGES") { set.retain(|x| x != "SEND_MESSAGES" && x != "SEND_VOICE_MESSAGES"); }
        if !has(&set, "CONNECT") { set.retain(|x| x != "SPEAK" && x != "STREAM"); }
        self.limit(s, user, set)
    }
    pub fn can(&self, s: &Value, user: &str, perm: &str) -> bool { self.perms_of(s, user).iter().any(|p| p == perm) }
    pub fn can_in(&self, s: &Value, ch: Option<&Value>, user: &str, perm: &str) -> bool { self.perms_in(s, ch, user).iter().any(|p| p == perm) }
    /// Owner = infinity; else the highest role's position (0 without one).
    pub fn rank(&self, s: &Value, user: &str) -> f64 { if self.is_owner(s, user) { f64::INFINITY } else { self.roles_of(s, user).iter().map(|r| r["position"].as_f64().unwrap_or(0.0)).fold(0.0, f64::max) } }
    pub fn outranks(&self, s: &Value, user: &str, other: &str) -> bool { self.rank(s, user) > self.rank(s, other) }
    pub fn role_below(&self, s: &Value, user: &str, role: &Value) -> bool { self.rank(s, user) > role["position"].as_f64().unwrap_or(0.0) }
    pub fn grantable(&self, s: &Value, user: &str, perms: &[String]) -> bool { let mine = self.base(s, user); perms.iter().all(|p| mine.contains(p)) }
}
