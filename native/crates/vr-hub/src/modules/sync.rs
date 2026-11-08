//! The data bridge (T-324; the team note plans/dev-hub.md, section 2): two hubs on this machine (live and dev) each keep
//! their own full copy of the data, and this keeps the two the same, both ways, continuously. The work is
//! pgm-sync-internal's (PGM_Sync_Internal); here are the hub's adapters, one per kind of record:
//!   board.task      a task as stored (tombstones inside it), newest by rev then updatedAt; a conflict's loser goes into the
//!                   activity log in full; two different tasks that got one number at once: the later one is renumbered
//!   board.activity  activity entries (append-only, the same entry once)        board.board  the boards (id + name)
//!   note            a note per path (text, time, who); a conflict keeps both: "<name> - <copy> copy.md"; deletes travel
//!   chat.message    messages by their mid, each hub numbering them itself; edits / deletes by revision (delete > edit)
//!   chat.key        each person's chat key (public key + password-wrapped backup), so every message opens on both
//!   chat.seen       read receipts, moved forward only
//!   account         per account and field: auth (the password hash: one password signs in on both), config, prefs,
//!                   profile, presence; newest wins. Sign-in sessions and device tokens never travel (each domain
//!                   signs in on its own); a deleted account is not deleted on the other hub
//!   device.lock     each account's device lock, newest change wins: a device known on one hub signs in on the other
//!                   (the page keeps one device id for portal.* and dev.portal.*), and "allow one more device" on
//!                   either hub counts on both
//!   avatar          profile pictures, by content
//!   work.archive    saved Work transcripts (Past sessions), per person and id; deletes travel
//! Not synced yet: chat attachments (the files behind a message), the servers / channels / groups (Chat/guilds.json), and the
//! live Work session records (work.json, work-sessions/: a session's harness state belongs to the hub that runs it).
//! Config: `"sync": { "peer": "http://127.0.0.1:<port>", "secretFile": "<path>", "name": "dev" }` (bridge/config.local.json);
//! no `sync` = no bridge. The routes /__sync/hello and /__sync/push answer only this machine, never through a proxy,
//! with the shared secret. Files: bridge/sync/ (this hub's own; the JavaScript hub never reads them).
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use super::Routed;
use crate::front::{json as jresp, Hub, Req};
use pgm_sync_internal::{canon, wire, Conflict, Engine, Kind};

pub struct Bridge { pub engine: Arc<Engine>, secret: String, pub peer: String }
impl Bridge {
    /// For /api/status (add-only).
    pub fn summary(&self) -> Value { let mut v = self.engine.status(); v["peerUrl"] = json!(self.peer); v }
}

/// Starts the bridge when config `sync` names a peer and a secret file with a secret in it.
pub fn start(hub: &Arc<Hub>) {
    let Some(c) = hub.cfg.get("sync").filter(|c| c.is_object()) else { return };
    let tag = "[vr-hub] sync:";
    let peer = c["peer"].as_str().unwrap_or("").trim().trim_end_matches('/').to_string();
    let local = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"].iter().any(|p| peer.strip_prefix(p).is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())));
    if !local { eprintln!("{tag} off: sync.peer must be http://127.0.0.1:<port> (the other hub on this machine), not \"{peer}\""); return; }
    let sf = c["secretFile"].as_str().unwrap_or("");
    let file = if Path::new(sf).is_absolute() { PathBuf::from(sf) } else { hub.root.join(sf) };
    let Some(secret) = (!sf.is_empty()).then(|| pgm_sync_internal::read_secret(&file)).flatten() else { eprintln!("{tag} off: sync.secretFile ({}) is missing or holds no secret of 16+ characters", file.display()); return; };
    let name = c["name"].as_str().or_else(|| hub.cfg.get("instance").and_then(Value::as_str)).filter(|n| !n.trim().is_empty()).map(|n| n.trim().chars().take(20).collect()).unwrap_or_else(|| format!("port{}", hub.cfg.port));
    let w = Arc::downgrade(hub);
    let kinds: Vec<Arc<dyn Kind>> = vec![
        Arc::new(Tasks(w.clone())), Arc::new(Activity(w.clone(), Mutex::default())), Arc::new(Boards(w.clone())), Arc::new(Notes(w.clone())),
        Arc::new(Messages(w.clone())), Arc::new(Keys(w.clone())), Arc::new(Seen(w.clone())), Arc::new(Accounts(w.clone())), Arc::new(Avatars(w.clone())), Arc::new(Archive(w.clone())), Arc::new(Locks(w.clone())),
    ];
    let engine = Arc::new(Engine::open(&hub.root.join("sync"), &name, kinds, tag));
    println!("{tag} on: this hub is \"{name}\" ({}), its peer {peer}", engine.id());
    let b = Arc::new(Bridge { engine: engine.clone(), secret: secret.clone(), peer: peer.clone() });
    if hub.mods.sync.set(b).is_err() { return; }
    // the dev hub borrows live's AI hosts over the bridge (T-335, plans/dev-hub.md § 9): live's lists for all of this hub's
    // accounts, every few seconds, in memory only
    if hub.mods.instance.is_dev() {
        let w2 = Arc::downgrade(hub);
        hub.mods.ai.borrow_from(&peer, &secret, &name, Arc::new(move || w2.upgrade().map(|h| h.mods.auth.accounts().into_iter().map(|a| a.username).collect()).unwrap_or_default()));
    }
    let e = engine.clone();
    super::forever("sync scan", move || wire::scan_loop(e.clone()));
    super::forever("sync send", move || wire::send_loop(engine.clone(), peer.clone(), secret.clone()));
}

/// /__sync/<rest>: the peer hub only (this machine, no proxy, the secret). Without a bridge: 404, as before.
pub async fn route(hub: &Arc<Hub>, r: &Req, rest: &str) -> Routed {
    let Some(b) = hub.mods.sync.get().cloned() else { return Routed::Answer(jresp(404, &json!({ "error": "Not found" }))) };
    if let Err((st, why)) = wire::gate(r.peer, &|k| r.header(k).map(String::from), &b.secret) {
        eprintln!("[vr-hub] sync: refused {} /__sync/{rest} from {}: {why}", r.method, r.peer.ip());
        return Routed::Answer(jresp(st, &json!({ "error": why })));
    }
    if let Some(what) = rest.strip_prefix("ai/") { return ai_route(hub, r, what).await; }
    let (m, rest, q, body) = (r.method.to_string(), rest.to_string(), r.param("hub").unwrap_or_default(), r.body.clone());
    let e = b.engine.clone();
    let (st, v) = tokio::task::spawn_blocking(move || wire::answer(&e, &m, &rest, &q, &body)).await.unwrap_or((500, json!({ "error": "the sync failed inside the hub" })));
    Routed::Answer(jresp(st, &v))
}

/// Whom this hub lends its AI hosts to over the bridge (config `sync.shareAi`, T-335): on by default when the bridge is
/// set up (both hubs are ours, on this machine, behind the gate and the shared secret); `false` = nobody (the route answers
/// 404, as an older hub does); `{ "users": [...] }` = those accounts only.
enum Share { Off, All, Only(Vec<String>) }
fn share_of(hub: &Hub) -> Share {
    let v = hub.cfg.get("sync").map(|s| s["shareAi"].clone()).unwrap_or(Value::Null);
    match v {
        Value::Bool(false) => Share::Off,
        Value::Object(o) if o.get("users").is_some_and(Value::is_array) => Share::Only(o["users"].as_array().map(|a| a.iter().filter_map(|u| u.as_str().map(|u| u.trim().to_lowercase())).collect()).unwrap_or_default()),
        _ => Share::All,
    }
}
/// One borrowing call into the security log (actor = the account it is for, detail.via = the hub that asked). Work runs,
/// cancels, PC tools and refusals every time; listings and model calls once per account and kind per 10 minutes (a run
/// makes hundreds of model calls, and the listing is asked for every 3 s).
fn audit_peer(account: &str, action: &str, target: &str, detail: Value, every: bool) {
    static SEEN: std::sync::OnceLock<Mutex<std::collections::HashMap<String, u64>>> = std::sync::OnceLock::new();
    if !every {
        let now = pgm_core_internal::now_ms(); let key = format!("{account}|{action}|{target}");
        let mut m = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if m.get(&key).is_some_and(|t| now.saturating_sub(*t) < 600_000) { return; }
        m.insert(key, now);
    }
    println!("[vr-hub] sync: ai {action} for {} ({target}, via {})", if account.is_empty() { "the hub" } else { account }, detail["via"].as_str().unwrap_or(""));
    pgm_core_internal::audit::record(account, action, target, detail, "");
}
/// /__sync/ai/<what> (T-335, plans/dev-hub.md § 9): the dev hub borrowing this hub's AI hosts, already through the gate.
/// Every call names the account it is for: one this hub does not have (or that sync.shareAi does not lend to) is refused
/// (403, and logged); the rest is `ai_serve`, as the JavaScript hub's calls are, with that account's checks.
async fn ai_route(hub: &Arc<Hub>, r: &Req, what: &str) -> Routed {
    use pgm_ai_internal::ai_peer;
    let share = share_of(hub);
    if matches!(share, Share::Off) { return Routed::Answer(jresp(404, &json!({ "error": "This hub does not lend its AI hosts (sync.shareAi is off)." }))); }
    let via: String = r.header(ai_peer::PEER_HEADER).unwrap_or("dev").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(20).collect();
    let via = if via.is_empty() { "dev".to_string() } else { via };
    let known = |u: &str| super::auth::valid_username(u) && hub.mods.has(u) && match &share { Share::Only(l) => l.iter().any(|x| x == u), _ => true };
    let refuse = |account: &str, why: &str| {
        audit_peer("", "ai.peer.refused", account, json!({ "via": via, "call": what, "why": why }), true);
        Routed::Answer(jresp(403, &json!({ "error": why })))
    };
    let deny = |u: &str| if hub.mods.has(u) { format!("This hub does not lend its AI hosts to {u} (sync.shareAi).") } else { format!("Live does not know the account {u}.") };
    match what {
        "providers" if r.method == hyper::Method::GET => {
            let one = r.param("user").unwrap_or_default();
            let users: Vec<String> = if !one.is_empty() { vec![one.trim().to_lowercase()] } else { r.param("users").unwrap_or_default().split(',').map(|u| u.trim().to_lowercase()).filter(|u| !u.is_empty()).take(500).collect() };
            if !one.is_empty() && !known(&users[0]) { return refuse(&users[0], &deny(&users[0])); }
            for u in users.iter().filter(|u| known(u)) { audit_peer(u, "ai.peer.providers", "", json!({ "via": via }), false); }
            Routed::Answer(jresp(200, &ai_peer::providers_answer(&hub.mods.ai, &users, &known)))
        }
        "fetch" | "work" | "cancel" | "pc-tool" if r.method == hyper::Method::POST => {
            let (account, detail) = if what == "fetch" {
                use base64::Engine;
                let meta: Value = r.header(ai_peer::META_HEADER).and_then(|h| base64::engine::general_purpose::STANDARD.decode(h).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
                let mode = meta["mode"].as_str().unwrap_or("").to_string();
                (meta["account"].as_str().unwrap_or("").to_string(), json!({ "via": via, "mode": mode, "host": meta["host"], "provider": meta["provider"], "engine": meta["engine"], "use": meta["use"] }))
            } else {
                let b = r.json();
                (b["account"].as_str().unwrap_or("").to_string(), json!({ "via": via, "host": b["host"], "provider": b["provider"], "machine": b["machine"], "tool": b["name"], "id": b["id"], "project": b["payload"]["project"] }))
            };
            // Unity's engines are lent to the hub, not to a person: an engine call may name nobody (the hub itself)
            let engine = what == "fetch" && detail["mode"] == json!("engine");
            if !(engine && account.is_empty()) && !known(&account) { return refuse(&account, &deny(&account)); }
            let action = match what { "fetch" => "ai.peer.fetch", "work" => "ai.peer.work", "cancel" => "ai.peer.cancel", _ => "ai.peer.pc" };
            let target = if what == "fetch" { format!("{}{}", detail["mode"].as_str().unwrap_or(""), detail["host"].as_str().map(|h| format!(" {h}")).unwrap_or_default()) } else { detail["host"].as_str().or(detail["machine"].as_str()).unwrap_or("").to_string() };
            audit_peer(&account, action, &target, detail, what != "fetch");
            super::ai_serve(hub, r, what, Some(&ai_peer::Who { account, peer: via.clone() })).await
        }
        _ => Routed::Answer(jresp(404, &json!({ "error": "Not found" }))),
    }
}

/// The losing side of a conflict on a kind with no history of its own (accounts, keys, boards, pictures): kept in
/// bridge/sync/lost.jsonl (private, add-only), never thrown away.
fn keep_aside(w: &Weak<Hub>, kind: &str, id: &str, c: &Conflict) {
    let (Some(h), Some(l)) = (w.upgrade(), c.loser) else { return };
    use std::io::Write;
    let f = h.root.join("sync").join("lost.jsonl");
    if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(&f) {
        let _ = writeln!(fh, "{}", json!({ "at": pgm_core_internal::now_ms(), "kind": kind, "id": id, "kept": c.winner_from, "lostFrom": c.loser_from, "lost": l }));
        pgm_core_internal::fs::private(&f);
    }
}
fn ms_of_iso(v: &Value) -> Option<i64> { v.as_str().and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok()).map(|d| d.timestamp_millis()) }
fn read_obj(f: &Path) -> Option<Value> { std::fs::read_to_string(f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(Value::is_object) }
fn mtime_ms(f: &Path) -> i64 { std::fs::metadata(f).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0) }

// ---------- the board ----------
struct Tasks(Weak<Hub>);
impl Kind for Tasks {
    fn name(&self) -> &str { "board.task" }
    fn version(&self) -> Option<u64> { self.0.upgrade().map(|h| h.mods.board.version()) }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| h.mods.board.sync_tasks().into_iter().filter_map(|t| t["id"].as_str().map(|id| (id.to_string(), t.clone()))).collect()).unwrap_or_default() }
    fn get(&self, id: &str) -> Option<Value> { self.0.upgrade()?.mods.board.get(id) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> { super::board::sync_task_ok(id, v) }
    fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { Some((v["rev"].as_i64().unwrap_or(0), v["updatedAt"].as_i64().unwrap_or(0))) }
    fn identity(&self, v: &Value) -> Option<String> { Some(format!("{}|{}", v["createdAt"].as_i64().unwrap_or(0), v["createdBy"].as_str().unwrap_or(""))) }
    fn rekey(&self, id: &str) -> Option<String> { self.0.upgrade()?.mods.board.sync_rekey(id) }
    fn put(&self, _id: &str, v: &Value) -> Result<Value, String> { Ok(self.0.upgrade().ok_or("the hub is stopping")?.mods.board.sync_put_task(v)) }
    fn keep_loser(&self, id: &str, c: &Conflict) {
        let Some(h) = self.0.upgrade() else { return };
        if let Some(e) = super::board::sync_conflict_entry(id, c.winner, c.loser, c.winner_from, c.loser_from) { h.mods.board.sync_add_activity(&e); }
    }
}
/// Activity entries by the hash of their content (the same entry is the same record on both hubs); a cache of the ids.
struct Activity(Weak<Hub>, Mutex<(u64, std::collections::HashMap<String, Value>)>);
impl Activity {
    fn ids(&self, h: &Hub) -> std::collections::HashMap<String, Value> {
        let mut c = self.1.lock().unwrap_or_else(|e| e.into_inner());
        let ver = h.mods.board.version();
        if c.0 != ver || c.1.is_empty() { c.1 = h.mods.board.sync_activity().into_iter().map(|e| (canon::hash(&e)[..24].to_string(), e)).collect(); c.0 = ver; }
        c.1.clone()
    }
}
impl Kind for Activity {
    fn name(&self) -> &str { "board.activity" }
    fn version(&self) -> Option<u64> { self.0.upgrade().map(|h| h.mods.board.version()) }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| self.ids(&h).into_iter().collect()).unwrap_or_default() }
    fn get(&self, id: &str) -> Option<Value> { let h = self.0.upgrade()?; self.ids(&h).get(id).cloned() }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> {
        if !v.is_object() || !v["at"].is_number() || !v["what"].is_string() || !v["by"].is_string() { return Err("an activity entry has at, what and by".into()); }
        if canon::hash(v)[..24] != *id { return Err("its id is not its content's".into()); }
        if v.to_string().len() > 4_200_000 { return Err("too large".into()); }
        Ok(())
    }
    fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { Some((0, v["at"].as_i64().unwrap_or(0))) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        h.mods.board.sync_add_activity(v);
        let mut c = self.1.lock().unwrap_or_else(|e| e.into_inner());
        c.1.insert(id.to_string(), v.clone()); c.0 = h.mods.board.version();
        Ok(v.clone())
    }
}
struct Boards(Weak<Hub>);
impl Kind for Boards {
    fn name(&self) -> &str { "board.board" }
    fn version(&self) -> Option<u64> { self.0.upgrade().map(|h| h.mods.board.version()) }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| h.mods.board.sync_boards().into_iter().filter_map(|b| b["id"].as_str().map(|id| (id.to_string(), b.clone()))).collect()).unwrap_or_default() }
    fn get(&self, id: &str) -> Option<Value> { self.0.upgrade()?.mods.board.sync_boards().into_iter().find(|b| b["id"] == json!(id)) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> { if v["id"] == json!(id) && v["name"].as_str().is_some_and(|n| !n.trim().is_empty() && n.chars().count() <= 60) && id.len() <= 60 { Ok(()) } else { Err("a board is { id, name }".into()) } }
    fn keep_loser(&self, id: &str, c: &Conflict) { keep_aside(&self.0, "board.board", id, c) }
    fn put(&self, _id: &str, v: &Value) -> Result<Value, String> { self.0.upgrade().ok_or("the hub is stopping")?.mods.board.sync_put_board(v); Ok(v.clone()) }
}

// ---------- notes ----------
struct Notes(Weak<Hub>);
impl Kind for Notes {
    fn name(&self) -> &str { "note" }
    fn version(&self) -> Option<u64> { self.0.upgrade().map(|h| h.mods.notes.version()) }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| h.mods.notes.sync_list()).unwrap_or_default() }
    fn deletes(&self) -> bool { true }
    fn get(&self, id: &str) -> Option<Value> { self.0.upgrade()?.mods.notes.sync_get(id) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        if !h.mods.notes.sync_path_ok(id) { return Err("not a note's path (folders/name.md: letters, digits, spaces, dots, dashes, underscores)".into()); }
        if !v["text"].as_str().is_some_and(|t| t.chars().count() <= 2_000_000) || !v["mtime"].is_i64() || !v["by"].is_string() { return Err("a note is { text (2 MB at most), mtime, by }".into()); }
        Ok(())
    }
    fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { Some((0, v["mtime"].as_i64().unwrap_or(0))) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        h.mods.notes.sync_put(id, v["text"].as_str().unwrap_or(""), v["mtime"].as_i64().unwrap_or(0), v["by"].as_str().unwrap_or(""))?;
        Ok(h.mods.notes.sync_get(id).unwrap_or_else(|| v.clone()))
    }
    fn remove(&self, id: &str) -> Result<(), String> { self.0.upgrade().ok_or("the hub is stopping")?.mods.notes.sync_remove(id) }
    /// Both hubs changed a note at once: the newest stays at its path, the other is kept next to it as
    /// "<name> - <hub> copy.md" (the same file on both hubs, so it is kept once).
    fn keep_loser(&self, id: &str, c: &Conflict) {
        let (Some(h), Some(l)) = (self.0.upgrade(), c.loser) else { return };
        if c.winner.is_some_and(|w| w["text"] == l["text"]) { return; }
        let copy = super::notes::Notes::sync_copy_path(id, c.loser_from);
        if let Err(e) = h.mods.notes.sync_put(&copy, l["text"].as_str().unwrap_or(""), l["mtime"].as_i64().unwrap_or(0), l["by"].as_str().unwrap_or("")) { eprintln!("[vr-hub] sync: could not keep the other version of {id} as {copy}: {e}"); }
    }
}

// ---------- the chat ----------
struct Messages(Weak<Hub>);
impl Kind for Messages {
    fn name(&self) -> &str { "chat.message" }
    fn version(&self) -> Option<u64> { self.0.upgrade().map(|h| h.mods.chat.sync_rev().max(0) as u64) }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| h.mods.chat.sync_messages(0)).unwrap_or_default() }
    fn changed(&self, since: u64) -> Option<Vec<(String, Value)>> { Some(self.0.upgrade()?.mods.chat.sync_messages(since as i64)) }
    fn get(&self, id: &str) -> Option<Value> { self.0.upgrade()?.mods.chat.sync_message(id) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> { super::chat::sync_message_ok(id, v) }
    fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { Some(super::chat::sync_stamp(v)) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> { Ok(self.0.upgrade().ok_or("the hub is stopping")?.mods.chat.sync_put_message(id, v)) }
}
struct Keys(Weak<Hub>);
impl Kind for Keys {
    fn name(&self) -> &str { "chat.key" }
    fn version(&self) -> Option<u64> { self.0.upgrade().map(|h| h.mods.chat.keys_version()) }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| h.mods.chat.sync_keys()).unwrap_or_default() }
    fn get(&self, id: &str) -> Option<Value> { self.0.upgrade()?.mods.chat.sync_key(id) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> {
        let b64 = |x: &Value, n: usize| x.as_str().is_some_and(|s| !s.is_empty() && s.len() <= n && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"+/=_-".contains(&c)));
        let p = &v["pub"]; let k = &v["backup"];
        if !super::auth::valid_username(id) && id != pgm_hcom_internal::chat::AGENT { return Err("not a username".into()); }
        if !(p["kty"] == json!("EC") && p["crv"] == json!("P-256") && b64(&p["x"], 64) && b64(&p["y"], 64) && p.get("d").is_none()) { return Err("not a public P-256 key".into()); }
        if !(b64(&k["salt"], 64) && b64(&k["iv"], 64) && b64(&k["ct"], 4096)) { return Err("no key backup".into()); }
        Ok(())
    }
    fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { Some((0, ms_of_iso(&v["updated"]).unwrap_or(0))) }
    fn keep_loser(&self, id: &str, c: &Conflict) { keep_aside(&self.0, "chat.key", id, c) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> { self.0.upgrade().ok_or("the hub is stopping")?.mods.chat.sync_put_key(id, v); Ok(v.clone()) }
}
struct Seen(Weak<Hub>);
impl Kind for Seen {
    fn name(&self) -> &str { "chat.seen" }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| h.mods.chat.sync_seen()).unwrap_or_default() }
    fn get(&self, id: &str) -> Option<Value> { self.0.upgrade()?.mods.chat.sync_seen().into_iter().find(|(k, _)| k == id).map(|(_, v)| v) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> { if id.split_once('|').is_some_and(|(u, c)| super::auth::valid_username(u) && !c.is_empty()) && v["mid"].as_str().is_some_and(|m| !m.is_empty() && m.len() <= 300) { Ok(()) } else { Err("a receipt is \"user|conv\" → { mid }".into()) } }
    /// the later of the two (as this hub numbers the messages); a message not here yet keeps this hub's
    fn merge(&self, _id: &str, local: Option<&Value>, inc: &Value) -> Option<Value> {
        let h = self.0.upgrade()?;
        let at = |v: &Value| v["mid"].as_str().and_then(|m| h.mods.chat.sync_seq_of(m));
        Some(match local { Some(l) if at(inc).unwrap_or(-1) <= at(l).unwrap_or(-1) => l.clone(), _ => inc.clone() })
    }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        let (u, c) = id.split_once('|').ok_or("bad id")?;
        h.mods.chat.sync_put_seen(u, c, v["mid"].as_str().unwrap_or(""));
        Ok(self.get(id).unwrap_or_else(|| v.clone()))
    }
}

// ---------- accounts ----------
/// Per account and field: "<user>/auth" (auth.json: the password hash), "<user>/config" (a profile's config.json: its name,
/// admin), "<user>/prefs" / "profile" / "presence" (those fields of its state.json).
struct Accounts(Weak<Hub>);
const STATE_FIELDS: [&str; 3] = ["prefs", "profile", "presence"];
impl Accounts {
    fn default_user(h: &Hub) -> String { h.cfg.get("defaultUsername").and_then(Value::as_str).unwrap_or("mills").to_string() }
    /// The folder of an account (made for a profile that is new here); None for a name that cannot be one.
    fn dir(h: &Hub, u: &str, make: bool) -> Option<PathBuf> {
        if let Some(a) = h.mods.auth.account(u) { return Some(a.dir); }
        if !super::auth::valid_username(u) || u == Self::default_user(h) { return None; }
        let d = h.root.join("profiles").join(u);
        if make { std::fs::create_dir_all(&d).ok()?; }
        Some(d)
    }
    fn field(h: &Hub, u: &str, f: &str) -> Option<Value> {
        let a = h.mods.auth.account(u)?;
        match f {
            "auth" => read_obj(&a.dir.join("auth.json")),
            "config" if a.dir != h.root => read_obj(&a.dir.join("config.json")),
            f if STATE_FIELDS.contains(&f) => read_obj(&a.dir.join("state.json")).map(|s| s[f].clone()).filter(Value::is_object),
            _ => None,
        }
    }
}
impl Kind for Accounts {
    fn name(&self) -> &str { "account" }
    fn snapshot(&self) -> Vec<(String, Value)> {
        let Some(h) = self.0.upgrade() else { return vec![] };
        let mut out = vec![];
        for a in h.mods.auth.accounts() { for f in ["auth", "config", "prefs", "profile", "presence"] { if let Some(v) = Self::field(&h, &a.username, f) { out.push((format!("{}/{f}", a.username), v)); } } }
        out
    }
    fn get(&self, id: &str) -> Option<Value> { let h = self.0.upgrade()?; let (u, f) = id.split_once('/')?; Self::field(&h, u, f) }
    /// When its file last changed (not when this hub first looked: two hubs pairing for the first time keep the newer password)
    fn stamp(&self, id: &str, _v: &Value) -> Option<(i64, i64)> {
        let h = self.0.upgrade()?; let (u, f) = id.split_once('/')?; let a = h.mods.auth.account(u)?;
        let file = match f { "auth" => "auth.json", "config" => "config.json", _ => "state.json" };
        Some((0, mtime_ms(&a.dir.join(file))))
    }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> {
        let (u, f) = id.split_once('/').ok_or("an account record is <user>/<field>")?;
        if !super::auth::valid_username(u) { return Err("not a username".into()); }
        if !v.is_object() || v.to_string().len() > 200_000 { return Err("not an object (or too large)".into()); }
        match f {
            "auth" => if v["hash"].as_str().is_some_and(|x| !x.is_empty()) && v["salt"].as_str().is_some_and(|x| !x.is_empty()) && v["N"].is_number() { Ok(()) } else { Err("a password record is { N, r, p, salt, hash }".into()) },
            "config" => if v.get("user").is_none_or(Value::is_string) && v.get("admin").is_none_or(Value::is_boolean) { Ok(()) } else { Err("a profile's config has user (text) and admin (true / false)".into()) },
            f if STATE_FIELDS.contains(&f) => Ok(()),
            _ => Err(format!("an unknown account field \"{f}\"")),
        }
    }
    fn keep_loser(&self, id: &str, c: &Conflict) { keep_aside(&self.0, "account", id, c) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        let (u, f) = id.split_once('/').ok_or("bad id")?;
        let dir = Self::dir(&h, u, true).ok_or("not an account this hub can hold")?;
        match f {
            "auth" => pgm_core_internal::json::write_json_private(&dir.join("auth.json"), v),
            "config" => { if dir == h.root { return Err("the default account's settings are this hub's own".into()); } pgm_core_internal::json::write_json(&dir.join("config.json"), v) }
            _ => { let file = dir.join("state.json"); let mut st = read_obj(&file).unwrap_or(json!({})); st[f] = v.clone(); pgm_core_internal::json::write_json(&file, &st); }
        }
        h.mods.chat.keys_changed(); // names, profiles and presence settings are read again
        Ok(Self::field(&h, u, f).unwrap_or_else(|| v.clone()))
    }
}

/// Of two saves of one transcript, the newer (savedAt, then the bigger content as a tie-break): the same on both hubs.
fn archive_pick<'a>(a: &'a Value, b: &'a Value) -> (&'a Value, &'a Value) {
    let key = |v: &Value| (ms_of_iso(&v["savedAt"]).unwrap_or(0), v["transcript"].as_array().map(|t| t.len()).unwrap_or(0), canon::canonical(v));
    if key(b) > key(a) { (b, a) } else { (a, b) }
}
/// Does `loser` hold a turn (who + text) that `winner` does not (counting repeats)?
fn has_turns_beyond(loser: &Value, winner: &Value) -> bool {
    let turn = |t: &Value| (t["cls"].as_str().unwrap_or("").to_string(), t["text"].as_str().unwrap_or("").to_string());
    let mut have: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
    for t in winner["transcript"].as_array().into_iter().flatten() { *have.entry(turn(t)).or_default() += 1; }
    loser["transcript"].as_array().into_iter().flatten().any(|t| match have.get_mut(&turn(t)) { Some(n) if *n > 0 => { *n -= 1; false } _ => true })
}

// ---------- device locks ----------
/// Per account: its device lock (bridge/device-locks.json), so a device that joined on one hub signs in on the other and an
/// admin's "allow one more device" on either hub lets the next new device in on both. The two copies of one lock (the same
/// claim, `at`) are merged: every device either hub knows, and `allowNext` as the newer change (`changedAt`) has it. A lock
/// claimed later (its account's lock was reset, then a device claimed it again) replaces the older one. A reset alone does
/// not travel (an account deleted on dev must not unlock it on live): the claim after it does.
struct Locks(Weak<Hub>);
fn lock_hash_ok(d: &Value) -> bool { d.as_str().is_some_and(|x| x.len() == 64 && x.bytes().all(|b| b.is_ascii_hexdigit())) }
impl Kind for Locks {
    fn name(&self) -> &str { "device.lock" }
    fn version(&self) -> Option<u64> { self.0.upgrade().map(|h| h.mods.auth.locks_version()) }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| h.mods.auth.sync_locks()).unwrap_or_default() }
    fn get(&self, id: &str) -> Option<Value> { self.0.upgrade()?.mods.auth.sync_lock(id) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> {
        let devices_ok = v.get("devices").is_none_or(|d| d.as_array().is_some_and(|a| a.len() < pgm_identity_internal::MAX_DEVICES && a.iter().all(|x| lock_hash_ok(&x["device"]))));
        if super::auth::valid_username(id) && lock_hash_ok(&v["device"]) && devices_ok && v.get("allowNext").is_none_or(Value::is_boolean) && v.get("changedAt").is_none_or(Value::is_i64) { Ok(()) }
        else { Err("a device lock is { device, at, devices?: [{ device, at }], allowNext?, changedAt? } (devices as sha256 hex)".into()) }
    }
    fn merge(&self, _id: &str, local: Option<&Value>, inc: &Value) -> Option<Value> { Some(merge_locks(local, inc)) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> { self.0.upgrade().ok_or("the hub is stopping")?.mods.auth.sync_put_lock(id, v); Ok(v.clone()) }
}
/// Two copies of one account's lock as one, the same whichever side merges.
fn merge_locks(local: Option<&Value>, inc: &Value) -> Value {
    let Some(l) = local else { return inc.clone() };
    let claimed = |v: &Value| ms_of_iso(&v["at"]).unwrap_or(0);
    if claimed(l) != claimed(inc) { return if claimed(l) > claimed(inc) { l.clone() } else { inc.clone() }; }
    let mut all = std::collections::BTreeMap::<String, Value>::new(); // device → when it joined
    for v in [l, inc] {
        for (d, at) in std::iter::once((&v["device"], &v["at"])).chain(v["devices"].as_array().into_iter().flatten().map(|x| (&x["device"], &x["at"]))) {
            if let Some(d) = d.as_str() { all.entry(d.to_string()).or_insert_with(|| at.clone()); }
        }
    }
    let first = if l["device"] == inc["device"] { l["device"].as_str() } else { l["device"].as_str().into_iter().chain(inc["device"].as_str()).min() }.unwrap_or("").to_string();
    let mut rest: Vec<(String, Value)> = all.into_iter().filter(|(d, _)| *d != first).collect();
    rest.sort_by(|a, b| (a.1.as_str().unwrap_or(""), &a.0).cmp(&(b.1.as_str().unwrap_or(""), &b.0)));
    rest.truncate(pgm_identity_internal::MAX_DEVICES - 1);
    let (cl, ci) = (l["changedAt"].as_i64().unwrap_or(0), inc["changedAt"].as_i64().unwrap_or(0));
    let allow = |v: &Value| v["allowNext"] == json!(true);
    let allow_next = if cl != ci { allow(if cl > ci { l } else { inc }) } else { allow(l) || allow(inc) };
    let mut out = json!({ "device": first, "at": l["at"] });
    if !rest.is_empty() { out["devices"] = json!(rest.into_iter().map(|(d, at)| json!({ "device": d, "at": at })).collect::<Vec<_>>()); }
    if allow_next { out["allowNext"] = json!(true); }
    if cl.max(ci) > 0 { out["changedAt"] = json!(cl.max(ci)); }
    out
}

// ---------- Work: saved transcripts ----------
/// Past sessions (Archive/<id>.json for the default account, Archive/<user>/<id>.json for the others), as "<user>/<id>" →
/// the saved file. A running session is not one of these: it stays on the hub that runs it.
struct Archive(Weak<Hub>);
impl Archive {
    fn default_user(h: &Hub) -> String { h.cfg.get("defaultUsername").and_then(Value::as_str).unwrap_or("mills").to_string() }
    fn dir_of(h: &Hub, u: &str) -> PathBuf { let base = h.mods.mills_dir().join("Archive"); if u == Self::default_user(h) { base } else { base.join(u) } }
    fn id_ok(id: &str) -> bool { id.len() == 16 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }
    /// (user, folder) for every account that may have saved sessions
    fn folders(h: &Hub) -> Vec<(String, PathBuf)> {
        let mut v = vec![(Self::default_user(h), Self::dir_of(h, &Self::default_user(h)))];
        for e in std::fs::read_dir(h.mods.mills_dir().join("Archive")).into_iter().flatten().filter_map(|e| e.ok()) {
            let n = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() && super::auth::valid_username(&n) && n != v[0].0 { v.push((n, e.path())); }
        }
        v
    }
    fn files(h: &Hub) -> Vec<(String, PathBuf)> {
        let mut out = vec![];
        for (u, d) in Self::folders(h) {
            for e in std::fs::read_dir(&d).into_iter().flatten().filter_map(|e| e.ok()) {
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(id) = n.strip_suffix(".json").filter(|i| Self::id_ok(i)) { out.push((format!("{u}/{id}"), e.path())); }
            }
        }
        out
    }
}
impl Kind for Archive {
    fn name(&self) -> &str { "work.archive" }
    /// the files' names, sizes and times: read again only when one of them changed
    fn version(&self) -> Option<u64> {
        let h = self.0.upgrade()?;
        let mut sig: Vec<String> = Self::files(&h).into_iter().map(|(id, f)| { let m = std::fs::metadata(&f).ok(); format!("{id}:{}:{}", m.as_ref().map(|m| m.len()).unwrap_or(0), mtime_ms(&f)) }).collect();
        sig.sort();
        Some(u64::from_str_radix(&canon::hash(&json!(sig))[..15], 16).unwrap_or(0))
    }
    fn snapshot(&self) -> Vec<(String, Value)> { self.0.upgrade().map(|h| Self::files(&h).into_iter().filter_map(|(id, f)| read_obj(&f).map(|v| (id, v))).collect()).unwrap_or_default() }
    fn deletes(&self) -> bool { true }
    fn get(&self, id: &str) -> Option<Value> { let h = self.0.upgrade()?; let (u, i) = id.split_once('/')?; read_obj(&Self::dir_of(&h, u).join(format!("{i}.json"))) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> {
        let (u, i) = id.split_once('/').ok_or("a saved session is <user>/<id>")?;
        if !super::auth::valid_username(u) || !Self::id_ok(i) { return Err("not <user>/<16 hex digits>".into()); }
        if v["id"].as_str() != Some(i) || !v["transcript"].is_array() || !v["title"].is_string() { return Err("a saved session is { id, title, savedAt, transcript: [...] }".into()); }
        if v.to_string().len() > 64_000_000 { return Err("too large".into()); }
        Ok(())
    }
    fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { Some((0, ms_of_iso(&v["savedAt"]).unwrap_or(0))) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        let (u, i) = id.split_once('/').ok_or("bad id")?;
        let d = Self::dir_of(&h, u); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        pgm_core_internal::json::write_json(&d.join(format!("{i}.json")), v);
        Ok(self.get(id).unwrap_or_else(|| v.clone()))
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        let (u, i) = id.split_once('/').ok_or("bad id")?;
        let _ = std::fs::remove_file(Self::dir_of(&h, u).join(format!("{i}.json")));
        Ok(())
    }
    fn keep_loser(&self, id: &str, c: &Conflict) { keep_aside(&self.0, "work.archive", id, c) }
    /// Both hubs may save one session's transcript (a session continued on dev while it runs on live: 2026-10-31,
    /// T-329). The newer save still wins on both, but a losing copy with turns the winner lacks is set aside (lost.jsonl)
    /// even when it was no conflict by the engine's rule: no turn ever vanishes without a trace.
    fn merge(&self, id: &str, local: Option<&Value>, inc: &Value) -> Option<Value> {
        let l = local?;
        let (winner, loser) = archive_pick(l, inc);
        if has_turns_beyond(loser, winner) { keep_aside(&self.0, "work.archive", id, &Conflict { winner: Some(winner), loser: Some(loser), winner_from: "the newer save", loser_from: "the older save" }); }
        Some(winner.clone())
    }
}

// ---------- pictures ----------
/// Profile pictures (Avatars/<user>.<webp|png|jpg>): { ext, data (base64), mtime }.
struct Avatars(Weak<Hub>);
const PIC: [&str; 3] = ["webp", "png", "jpg"];
impl Avatars {
    fn dir(h: &Hub) -> PathBuf { h.mods.mills_dir().join("Avatars") }
    fn read(h: &Hub, u: &str) -> Option<Value> {
        PIC.iter().find_map(|e| { let f = Self::dir(h).join(format!("{u}.{e}")); std::fs::read(&f).ok().map(|b| json!({ "ext": e, "data": pgm_core_internal::b64(&b), "mtime": mtime_ms(&f) })) })
    }
}
impl Kind for Avatars {
    fn name(&self) -> &str { "avatar" }
    fn version(&self) -> Option<u64> { self.0.upgrade().map(|h| h.mods.chat.keys_version()) }
    fn snapshot(&self) -> Vec<(String, Value)> {
        let Some(h) = self.0.upgrade() else { return vec![] };
        let names: std::collections::BTreeSet<String> = std::fs::read_dir(Self::dir(&h)).map(|r| r.filter_map(|e| e.ok()).filter_map(|e| { let n = e.file_name().to_string_lossy().to_string(); let (u, ext) = n.rsplit_once('.')?; (PIC.contains(&ext) && super::auth::valid_username(u)).then(|| u.to_string()) }).collect()).unwrap_or_default();
        names.into_iter().filter_map(|u| Self::read(&h, &u).map(|v| (u, v))).collect()
    }
    fn deletes(&self) -> bool { true }
    fn get(&self, id: &str) -> Option<Value> { let h = self.0.upgrade()?; Self::read(&h, id) }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> {
        if !super::auth::valid_username(id) { return Err("not a username".into()); }
        if !v["ext"].as_str().is_some_and(|e| PIC.contains(&e)) || !v["mtime"].is_i64() { return Err("a picture is { ext: webp|png|jpg, data, mtime }".into()); }
        match v["data"].as_str().map(pgm_core_internal::unb64) { Some(Ok(b)) if !b.is_empty() && b.len() <= 300 * 1024 => Ok(()), _ => Err("the picture is not base64 or over 300 KB".into()) }
    }
    fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { Some((0, v["mtime"].as_i64().unwrap_or(0))) }
    fn keep_loser(&self, id: &str, c: &Conflict) { keep_aside(&self.0, "avatar", id, c) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        let dir = Self::dir(&h); std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let ext = v["ext"].as_str().unwrap_or("png");
        let buf = pgm_core_internal::unb64(v["data"].as_str().unwrap_or("")).map_err(|e| e.to_string())?;
        for e in PIC { if e != ext { let _ = std::fs::remove_file(dir.join(format!("{id}.{e}"))); } }
        let f = dir.join(format!("{id}.{ext}")); let tmp = f.with_extension("sync-tmp");
        std::fs::write(&tmp, &buf).map_err(|e| e.to_string())?;
        if let Ok(fh) = std::fs::File::options().write(true).open(&tmp) { let _ = fh.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(v["mtime"].as_i64().unwrap_or(0).max(0) as u64)); }
        std::fs::rename(&tmp, &f).map_err(|e| e.to_string())?;
        h.mods.chat.keys_changed();
        Ok(Self::read(&h, id).unwrap_or_else(|| v.clone()))
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        let h = self.0.upgrade().ok_or("the hub is stopping")?;
        for e in PIC { let _ = std::fs::remove_file(Self::dir(&h).join(format!("{id}.{e}"))); }
        h.mods.chat.keys_changed();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::auth::lock_devices;
    fn h(c: char) -> String { c.to_string().repeat(64) }
    #[test]
    fn a_transcript_saved_on_both_hubs_keeps_the_newer_and_sets_aside_turns_it_lacks() {
        let t = |v: &[(&str, &str)]| json!(v.iter().map(|(c, x)| json!({ "cls": c, "text": x })).collect::<Vec<_>>());
        let dev = json!({ "id": "ab", "savedAt": "2026-10-31T00:20:00.000Z", "transcript": t(&[("me", "hi"), ("claude", "hello"), ("me", "Its in, and I am on dev now")]) });
        let live = json!({ "id": "ab", "savedAt": "2026-10-31T00:39:00.000Z", "transcript": t(&[("me", "hi"), ("claude", "hello"), ("me", "next"), ("claude", "done")]) });
        // the same winner whichever hub decides
        assert_eq!(archive_pick(&dev, &live).0, &live); assert_eq!(archive_pick(&live, &dev).0, &live);
        // dev's copy has a turn live's lacks: it is set aside; a plain longer save of the same conversation is not
        assert!(has_turns_beyond(&dev, &live));
        let shorter = json!({ "savedAt": "2026-10-31T00:10:00.000Z", "transcript": t(&[("me", "hi"), ("claude", "hello")]) });
        assert!(!has_turns_beyond(&shorter, &live));
        // a repeated line counts: two "ok"s are not covered by one
        let two = json!({ "transcript": t(&[("me", "ok"), ("me", "ok")]) }); let one = json!({ "transcript": t(&[("me", "ok")]) });
        assert!(has_turns_beyond(&two, &one) && !has_turns_beyond(&one, &two));
    }
    #[test]
    fn two_copies_of_one_lock_merge_to_the_same_lock_on_both_sides() {
        let live = json!({ "device": h('a'), "at": "2026-10-19T06:28:35.148Z", "allowNext": true });
        let dev = json!({ "device": h('a'), "at": "2026-10-19T06:28:35.148Z", "devices": [{ "device": h('b'), "at": "2026-10-29T10:00:00.000Z" }], "changedAt": 5 });
        let (x, y) = (merge_locks(Some(&live), &dev), merge_locks(Some(&dev), &live));
        assert_eq!(x, y);
        assert_eq!(lock_devices(&x), vec![h('a'), h('b')]);
        assert_eq!(x["allowNext"], Value::Null, "the newer change (dev's, which used the allow) wins it");
        assert_eq!(merge_locks(Some(&x), &x), x, "merging again changes nothing");
        // neither says when it changed (locks from before 5.34.3): an allow on either counts
        let old = json!({ "device": h('a'), "at": "2026-10-19T06:28:35.148Z" });
        assert_eq!(merge_locks(Some(&old), &live)["allowNext"], json!(true));
        assert_eq!(merge_locks(Some(&live), &old)["allowNext"], json!(true));
        // a lock claimed later (after a reset) replaces the older one whole, whichever side has it
        let again = json!({ "device": h('c'), "at": "2026-10-29T11:00:00.000Z", "changedAt": 9 });
        assert_eq!(merge_locks(Some(&dev), &again), again);
        assert_eq!(merge_locks(Some(&again), &dev), again);
        assert_eq!(merge_locks(None, &dev), dev);
    }
}
