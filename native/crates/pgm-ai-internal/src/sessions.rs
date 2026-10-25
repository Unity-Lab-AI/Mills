//! Many Work sessions per person (T-284; team note plans/multi-session.md). Each session has its own conversation +
//! queue, harness session, provider / model, project and its own worktree (T-292); several run at once.
//! - The first session (made from what a person had before) is the **primary** one: its state stays where it always
//!   was (<profile>/work.json, work-current.json), so the frozen JavaScript hub and older pages, which know nothing of
//!   sessions, see exactly what they saw before. A route called without a session means the primary one.
//! - Every other session: <profile>/work-sessions/<id>.json = { state: {work.json's keys}, current: {work-current.json's
//!   fields} }.
//! - The list: <profile>/work-sessions.json = { primary, current (the one open on screen), order, sessions: { id: meta } },
//!   meta = { title, createdAt, updatedAt, lastActivityAt, folder, pinned, hidden, archived, draft, draftAt, seenAt,
//!   lastReplyAt, waiting, worktree, ended }. The person's folders: <profile>/work-folders.json.
//! - A session's run is keyed by the person for the primary session (as before) and "<user>#<id>" for the others.
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::hub::{answer, AiHub, Resp};
use crate::work::Work;
use pgm_core_internal::json::{read_json, s, write_json};
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

/// Read-modify-write of the lists, one at a time.
static LOCK: Mutex<()> = Mutex::new(());
const IDLE_MS: u64 = 5 * 60_000;
const MAX_SESSIONS: usize = 200;

/// One session as a run sees it: its id, its run key, whether it is the primary one.
#[derive(Clone, Debug)]
pub struct Sx { pub sid: String, pub key: String, pub primary: bool }
impl Sx {
    /// The session's id without "s-" (its worktree / branch name: <user>.<hex>).
    pub fn hex(&self) -> &str { self.sid.trim_start_matches("s-") }
}

fn index_file(a: &Account) -> PathBuf { a.dir.join("work-sessions.json") }
fn folders_file(a: &Account) -> PathBuf { a.dir.join("work-folders.json") }
fn session_file(a: &Account, sid: &str) -> PathBuf { a.dir.join("work-sessions").join(format!("{sid}.json")) }
fn new_sid() -> String { format!("s-{}", pgm_core_internal::crypto::random_hex(6)) }
fn valid_sid(x: &str) -> bool { x.len() == 14 && x.starts_with("s-") && x[2..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) }
fn meta_new(title: &str) -> Value { let now = now_ms(); json!({ "title": title, "createdAt": now, "updatedAt": now, "lastActivityAt": now, "folder": Value::Null, "pinned": false, "hidden": false, "archived": false, "draft": "", "seenAt": now, "lastReplyAt": 0 }) }

/// The person's list (made the first time from what they had: one primary session).
pub fn index(a: &Account) -> Value {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    index_unlocked(a)
}
fn index_unlocked(a: &Account) -> Value {
    if let Some(v) = read_json(&index_file(a)).filter(|v| v["primary"].as_str().map(valid_sid).unwrap_or(false) && v["sessions"].is_object()) { return v; }
    let sid = new_sid();
    let title = read_json(&a.dir.join("work-current.json")).map(|c| s(&c, "archiveTitle")).unwrap_or_default();
    let v = json!({ "primary": sid, "current": sid, "order": [sid], "sessions": { sid.clone(): meta_new(&title) } });
    write_json(&index_file(a), &v);
    v
}
fn with_index<T>(a: &Account, f: impl FnOnce(&mut Value) -> T) -> T {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut v = index_unlocked(a);
    let out = f(&mut v);
    write_json(&index_file(a), &v);
    out
}
/// Changes one session's meta (nothing when it does not exist).
pub fn touch(a: &Account, sid: &str, f: impl FnOnce(&mut Map<String, Value>)) { with_index(a, |v| { if let Some(m) = v["sessions"].get_mut(sid).and_then(Value::as_object_mut) { f(m); m.insert("updatedAt".into(), json!(now_ms())); } }); }

/// The session a request names ("" = the primary one); None = no such session of theirs.
pub fn resolve(a: &Account, want: &str) -> Option<Sx> {
    let v = index(a); let primary = s(&v, "primary");
    let sid = if want.is_empty() { primary.clone() } else { want.to_string() };
    if v["sessions"].get(&sid).is_none() { return None; }
    let is_primary = sid == primary;
    Some(Sx { key: if is_primary { a.username.clone() } else { format!("{}#{}", a.username, sid) }, sid, primary: is_primary })
}
/// The session a request names: the body's "session", else ?session=.
pub fn wanted(r: &Req, b: &Value) -> String { b["session"].as_str().map(String::from).or_else(|| r.param("session")).unwrap_or_default() }

fn file_of(a: &Account, sid: &str) -> Value { read_json(&session_file(a, sid)).filter(Value::is_object).unwrap_or(json!({ "state": {}, "current": {} })) }
fn save_file(a: &Account, sid: &str, v: &Value) { let f = session_file(a, sid); let _ = std::fs::create_dir_all(f.parent().unwrap()); write_json(&f, v); }

/// A session's harness state (work.json's keys).
pub fn sstate(w: &Work, a: &Account, sx: &Sx) -> Value { if sx.primary { w.state(a) } else { let st = file_of(a, &sx.sid)["state"].clone(); if st.is_object() { st } else { json!({}) } } }
pub fn supdate(w: &Work, a: &Account, sx: &Sx, f: impl FnOnce(&mut Map<String, Value>)) -> Value {
    if sx.primary { return w.update_state(a, f); }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut v = file_of(a, &sx.sid); if !v["state"].is_object() { v["state"] = json!({}); }
    f(v["state"].as_object_mut().unwrap());
    save_file(a, &sx.sid, &v);
    v["state"].clone()
}
/// A session's conversation + queue (work-current.json's fields).
pub fn scurrent(a: &Account, sx: &Sx) -> Value { if sx.primary { crate::work::work_current(a) } else { crate::work::current_of(&file_of(a, &sx.sid)["current"]) } }
pub fn save_scurrent(a: &Account, sx: &Sx, st: &mut Value) {
    if sx.primary { crate::work::save_work_current(a, st); }
    else { st["updatedAt"] = json!(pgm_core_internal::time::iso_now()); let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner()); let mut v = file_of(a, &sx.sid); v["current"] = st.clone(); save_file(a, &sx.sid, &v); }
    touch(a, &sx.sid, |m| { m.insert("lastActivityAt".into(), json!(now_ms())); });
}

/// The state a session is in, the first that applies (plans/multi-session.md § 3).
fn state_of(m: &Value, running: bool, now: u64) -> &'static str {
    if !m["waiting"].is_null() && m["waiting"].is_object() { return "waiting"; }
    if running { return "working"; }
    if m["lastReplyAt"].as_u64().unwrap_or(0) > m["seenAt"].as_u64().unwrap_or(0) { return "done-unseen"; }
    if !s(m, "draft").is_empty() { return "draft"; }
    if now.saturating_sub(m["lastActivityAt"].as_u64().unwrap_or(0)) > IDLE_MS { return "idle"; }
    "ready"
}
/// The host a provider runs on ("hub" = this hub's own Claude).
pub fn host_of(provider: &str) -> String { provider.strip_prefix("h:").and_then(|x| x.split('/').next()).map(String::from).unwrap_or_else(|| "hub".into()) }

fn summary(w: &Work, a: &Account, v: &Value, sid: &str, now: u64) -> Value {
    let m = &v["sessions"][sid];
    let sx = Sx { sid: sid.to_string(), key: if sid == s(v, "primary") { a.username.clone() } else { format!("{}#{sid}", a.username) }, primary: sid == s(v, "primary") };
    let st = sstate(w, a, &sx);
    let running = w.busy_key(&sx.key);
    let provider = if s(&st, "provider").is_empty() { "host".to_string() } else { s(&st, "provider") };
    let project = { let p = s(&st, "project"); if p.is_empty() { pgm_collab_internal::worktree::HUB_PROJECT.to_string() } else { p } };
    let mut out = json!({ "id": sid, "title": m["title"], "state": state_of(m, running, now), "running": running, "primary": sx.primary, "host": host_of(&provider), "provider": provider, "model": s(&st, "model"), "project": project,
        "harness": if s(&st, "sessionHarness").is_empty() { "claude".to_string() } else { s(&st, "sessionHarness") }, "folder": m["folder"], "pinned": m["pinned"] == json!(true), "hidden": m["hidden"] == json!(true), "archived": m["archived"] == json!(true),
        "updatedAt": m["updatedAt"], "lastActivityAt": m["lastActivityAt"], "hasDraft": !s(m, "draft").is_empty(), "worktree": m["worktree"] });
    if m["ended"].is_object() { out["ended"] = m["ended"].clone(); }
    out
}
/// GET /api/work/sessions: the list, the folders, what is running.
fn list(w: &Work, a: &Account, archived: bool) -> Value {
    let v = index(a); let now = now_ms();
    let mut ids: Vec<String> = v["order"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(String::from)).filter(|x| v["sessions"].get(x).is_some()).collect();
    for k in v["sessions"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default() { if !ids.contains(&k) { ids.push(k); } }
    let all: Vec<Value> = ids.iter().map(|sid| summary(w, a, &v, sid, now)).collect();
    let working = all.iter().filter(|x| x["state"] == json!("working")).count(); let waiting = all.iter().filter(|x| x["state"] == json!("waiting")).count();
    let mut per_host: Map<String, Value> = Map::new();
    for x in all.iter().filter(|x| x["running"] == json!(true)) {
        let h = s(x, "host"); let e = per_host.entry(h).or_insert(json!({ "used": 0, "providers": {} }));
        e["used"] = json!(e["used"].as_u64().unwrap_or(0) + 1);
        let p = s(x, "provider"); e["providers"][&p] = json!(e["providers"][&p].as_u64().unwrap_or(0) + 1);
    }
    for (h, e) in per_host.iter_mut() { let (mu, mp) = w.session_limits(h); e["max"] = json!(mu); e["maxPerProvider"] = json!(mp); }
    let sessions: Vec<Value> = all.into_iter().filter(|x| archived || x["archived"] != json!(true)).collect();
    json!({ "current": v["current"], "primary": v["primary"], "sessions": sessions, "folders": folders(a), "counts": { "working": working, "waiting": waiting, "perHost": per_host } })
}
fn folders(a: &Account) -> Value { read_json(&folders_file(a)).map(|v| v["folders"].clone()).filter(Value::is_array).unwrap_or(json!([])) }
fn save_folders(a: &Account, f: &Value) { write_json(&folders_file(a), &json!({ "folders": f })); }
fn one_line(v: &Value, max: usize) -> String { v.as_str().unwrap_or("").chars().filter(|c| !c.is_control()).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect() }

/// The session routes (signed in as `a`); None = not one of them.
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    let p = r.path.as_str();
    if !(p.starts_with("/api/work/sessions") || p.starts_with("/api/work/folders") || p == "/api/work/draft") { return None; }
    let w = hub.work(); let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let b = r.json(); let id = s(&b, "id");
    let exists = |sid: &str| index(a)["sessions"].get(sid).is_some();
    Some(match p {
        "/api/work/sessions" if get => answer(200, list(w, a, r.param("archived").as_deref() == Some("1"))),
        "/api/work/sessions/new" if post => {
            let v = index(a);
            if v["sessions"].as_object().map(|o| o.len()).unwrap_or(0) >= MAX_SESSIONS { return Some(answer(400, json!({ "error": format!("You have {MAX_SESSIONS} sessions; delete some first.") }))); }
            let folder = b["folder"].as_str().filter(|f| folders(a).as_array().map(|l| l.iter().any(|x| x["id"] == json!(f))).unwrap_or(false)).map(|f| json!(f)).unwrap_or(Value::Null);
            // what is not given comes from the session open now
            let from = resolve(a, &s(&v, "current")).or_else(|| resolve(a, "")).map(|sx| sstate(w, a, &sx)).unwrap_or(json!({}));
            let mut st = Map::new();
            for k in ["provider", "model", "project"] { let x = if b.get(k).is_some() { s(&b, k) } else { s(&from, k) }; if !x.is_empty() { st.insert(k.into(), json!(x)); } }
            if let Some(pj) = st.get("project").and_then(Value::as_str) { if w.wt.project(pj).is_err() { return Some(answer(400, json!({ "error": "Unknown project." }))); } }
            let sid = new_sid();
            save_file(a, &sid, &json!({ "state": st, "current": {} }));
            let background = b["background"] == json!(true);
            let mut meta = meta_new(&one_line(&b["title"], 120)); meta["folder"] = folder;
            with_index(a, |v| { v["sessions"][&sid] = meta; let mut order = vec![json!(sid)]; order.extend(v["order"].as_array().cloned().unwrap_or_default()); v["order"] = json!(order); if !background { v["current"] = json!(sid); } });
            answer(200, json!({ "id": sid }))
        }
        "/api/work/sessions/open" if post => {
            if !exists(&id) { return Some(answer(404, json!({ "error": "No such session." }))); }
            with_index(a, |v| { v["current"] = json!(id); if let Some(m) = v["sessions"].get_mut(&id) { m["hidden"] = json!(false); m["archived"] = json!(false); m["seenAt"] = json!(now_ms()); } });
            answer(200, json!({ "current": id }))
        }
        "/api/work/sessions/rename" | "/api/work/sessions/pin" | "/api/work/sessions/seen" | "/api/work/sessions/hide" | "/api/work/sessions/move" if post => {
            if !exists(&id) { return Some(answer(404, json!({ "error": "No such session." }))); }
            let what = p.rsplit('/').next().unwrap_or("");
            if what == "move" && !b["folder"].is_null() && !folders(a).as_array().map(|l| l.iter().any(|x| x["id"] == b["folder"])).unwrap_or(false) { return Some(answer(400, json!({ "error": "No such folder." }))); }
            touch(a, &id, |m| match what {
                "rename" => { m.insert("title".into(), json!(one_line(&b["title"], 120))); }
                "pin" => { m.insert("pinned".into(), json!(b["pinned"] != json!(false))); }
                "seen" => { m.insert("seenAt".into(), json!(now_ms())); }
                "hide" => { m.insert("hidden".into(), json!(b["hidden"] != json!(false))); }
                _ => { m.insert("folder".into(), if b["folder"].is_null() { Value::Null } else { b["folder"].clone() }); }
            });
            answer(200, json!({ "ok": true }))
        }
        "/api/work/sessions/archive" if post => {
            let Some(sx) = resolve(a, &id).filter(|_| !id.is_empty()) else { return Some(answer(404, json!({ "error": "No such session." }))) };
            if w.busy_key(&sx.key) { return Some(answer(409, json!({ "error": "That session is still working. Stop it first, then archive it." }))); }
            let cur = scurrent(a, &sx); let st = sstate(w, a, &sx);
            let tr = cur["transcript"].as_array().cloned().unwrap_or_default();
            let mut saved = Value::Null;
            if !tr.is_empty() {
                let title = index(a)["sessions"][&sx.sid]["title"].as_str().filter(|t| !t.is_empty()).map(String::from).or_else(|| cur["archiveTitle"].as_str().map(String::from)).unwrap_or_default();
                saved = json!(crate::work::archive_put(a, &hub.mills_dir(), hub.root(), &s(&cur, "archiveId"), &title, &s(&st, "claudeSessionId"), &tr));
            }
            touch(a, &sx.sid, |m| { m.insert("archived".into(), json!(true)); });
            answer(200, json!({ "ok": true, "archiveId": saved }))
        }
        "/api/work/sessions/delete" if post => {
            let Some(sx) = resolve(a, &id).filter(|_| !id.is_empty()) else { return Some(answer(404, json!({ "error": "No such session." }))) };
            if sx.primary { return Some(answer(400, json!({ "error": "Your first session can be hidden or archived, not deleted (older pages and the fallback hub use it)." }))); }
            // its running agents are stopped first (plans/multi-session.md § 4)
            if w.busy_key(&sx.key) { w.stop(&**hub, &sx.key); for _ in 0..100 { if !w.busy_key(&sx.key) { break; } tokio::time::sleep(std::time::Duration::from_millis(100)).await; } }
            let project = { let p = s(&sstate(w, a, &sx), "project"); if p.is_empty() { pgm_collab_internal::worktree::HUB_PROJECT.to_string() } else { p } };
            let wt = &w.wt; let (u, hex) = (a.username.clone(), sx.hex().to_string());
            let tree = tokio::task::block_in_place(|| wt.remove_session(&u, &hex, &project));
            let _ = std::fs::remove_file(session_file(a, &sx.sid));
            with_index(a, |v| { if let Some(o) = v["sessions"].as_object_mut() { o.remove(&sx.sid); } let order: Vec<Value> = v["order"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| x != &json!(sx.sid)).collect(); v["order"] = json!(order); if v["current"] == json!(sx.sid) { v["current"] = v["primary"].clone(); } });
            answer(200, json!({ "ok": true, "worktree": tree }))
        }
        "/api/work/draft" if post => {
            let Some(sx) = resolve(a, &wanted(r, &b)) else { return Some(answer(404, json!({ "error": "No such session." }))) };
            let d: String = b["draft"].as_str().unwrap_or("").chars().take(50 * 1024).collect();
            touch(a, &sx.sid, |m| { m.insert("draft".into(), json!(d)); m.insert("draftAt".into(), json!(now_ms())); m.insert("lastActivityAt".into(), json!(now_ms())); });
            answer(200, json!({ "ok": true }))
        }
        "/api/work/folders" if get => answer(200, json!({ "folders": folders(a) })),
        "/api/work/folders/new" if post => {
            let name = one_line(&b["name"], 60); if name.is_empty() { return Some(answer(400, json!({ "error": "A folder needs a name." }))); }
            let mut f = folders(a); let fid = format!("f-{}", pgm_core_internal::crypto::random_hex(4));
            let n = f.as_array().map(|x| x.len()).unwrap_or(0);
            if n >= 100 { return Some(answer(400, json!({ "error": "That is a lot of folders (100); delete some first." }))); }
            f.as_array_mut().unwrap().push(json!({ "id": fid, "name": name, "order": n, "collapsed": false })); save_folders(a, &f);
            answer(200, json!({ "id": fid }))
        }
        "/api/work/folders/rename" if post => {
            let name = one_line(&b["name"], 60); let mut f = folders(a);
            let Some(x) = f.as_array_mut().unwrap().iter_mut().find(|x| x["id"] == json!(id)) else { return Some(answer(404, json!({ "error": "No such folder." }))) };
            if name.is_empty() { return Some(answer(400, json!({ "error": "A folder needs a name." }))); }
            x["name"] = json!(name); if b.get("collapsed").is_some() { x["collapsed"] = json!(b["collapsed"] == json!(true)); } save_folders(a, &f);
            answer(200, json!({ "ok": true }))
        }
        "/api/work/folders/delete" if post => {
            let mut f = folders(a); let before = f.as_array().map(|x| x.len()).unwrap_or(0);
            f.as_array_mut().unwrap().retain(|x| x["id"] != json!(id));
            if f.as_array().map(|x| x.len()).unwrap_or(0) == before { return Some(answer(404, json!({ "error": "No such folder." }))); }
            save_folders(a, &f);
            // its sessions move out (never deleted)
            with_index(a, |v| { if let Some(o) = v["sessions"].as_object_mut() { for m in o.values_mut() { if m["folder"] == json!(id) { m["folder"] = Value::Null; } } } });
            answer(200, json!({ "ok": true }))
        }
        "/api/work/folders/order" if post => {
            let ids: Vec<String> = b["ids"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(String::from)).collect();
            let mut f = folders(a);
            for x in f.as_array_mut().unwrap().iter_mut() { if let Some(i) = ids.iter().position(|k| x["id"] == json!(k)) { x["order"] = json!(i); } }
            f.as_array_mut().unwrap().sort_by_key(|x| x["order"].as_u64().unwrap_or(0)); save_folders(a, &f);
            answer(200, json!({ "folders": f }))
        }
        _ => answer(404, json!({ "error": "Unknown endpoint" })),
    })
}
