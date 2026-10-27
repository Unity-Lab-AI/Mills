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
//!                   profile, presence; newest wins. Sign-in sessions, device tokens and device locks never travel
//!                   (each domain signs in on its own); a deleted account is not deleted on the other hub
//!   avatar          profile pictures, by content
//! Not synced yet: chat attachments and Work session records / saved transcripts (see the T-324 report).
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
        Arc::new(Messages(w.clone())), Arc::new(Keys(w.clone())), Arc::new(Seen(w.clone())), Arc::new(Accounts(w.clone())), Arc::new(Avatars(w.clone())),
    ];
    let engine = Arc::new(Engine::open(&hub.root.join("sync"), &name, kinds, tag));
    println!("{tag} on: this hub is \"{name}\" ({}), its peer {peer}", engine.id());
    let b = Arc::new(Bridge { engine: engine.clone(), secret: secret.clone(), peer: peer.clone() });
    if hub.mods.sync.set(b).is_err() { return; }
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
    let (m, rest, q, body) = (r.method.to_string(), rest.to_string(), r.param("hub").unwrap_or_default(), r.body.clone());
    let e = b.engine.clone();
    let (st, v) = tokio::task::spawn_blocking(move || wire::answer(&e, &m, &rest, &q, &body)).await.unwrap_or((500, json!({ "error": "the sync failed inside the hub" })));
    Routed::Answer(jresp(st, &v))
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
