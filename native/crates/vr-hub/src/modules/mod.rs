//! The hub's modules that run here in Rust. Each one owns its routes and its data files outright (the JavaScript hub no
//! longer touches them); where the JavaScript hub still needs one of them, it asks it over the internal API
//! (/internal/front/…, with the front's secret), and where one of them needs the JavaScript hub, it asks it the same way.
//! The order they move in: board T-131. Moved so far: accounts & authentication (auth.rs).
use hyper::Response;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::Arc;

use crate::front::{json as jresp, Body, Hub, Req};

pub mod auth;
pub mod board;
pub mod files;
pub mod forgejo;
pub mod notes;

/// bridge/config.json overlaid with bridge/config.local.json, one level deep (bridge/common.js loadProfile).
#[derive(Clone, Debug)]
pub struct Config { pub raw: Map<String, Value>, pub port: u16 }
impl Config {
    pub fn load(root: &Path) -> Config {
        let read = |f: &str| -> Map<String, Value> { std::fs::read_to_string(root.join(f)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default() };
        let mut base = read("config.json");
        for (k, v) in read("config.local.json") {
            match (base.get_mut(&k), &v) { (Some(Value::Object(b)), Value::Object(l)) => { for (k2, v2) in l { b.insert(k2.clone(), v2.clone()); } } _ => { base.insert(k, v); } }
        }
        let port = base.get("port").and_then(Value::as_u64).unwrap_or(7777) as u16;
        Config { raw: base, port }
    }
    pub fn get(&self, k: &str) -> Option<&Value> { self.raw.get(k) }
}

pub enum Routed {
    /// answered here
    Answer(Response<Body>),
    /// on to the JavaScript hub, with these headers added (who is signed in)
    Pass(Vec<(String, String)>),
    /// a sign-in went through: the answer is made by the front (it tells the JavaScript hub first)
    SignedIn { token: String, user: String, client: Value, ip: String },
    /// not a module's: on to the JavaScript hub as it is
    No,
}

pub struct Modules {
    pub front_secret: String,
    pub auth: auth::Auth,
    pub board: Arc<board::Board>,
    pub mirror: Arc<forgejo::Mirror>,
    pub notes: notes::Notes,
    pub files: files::Files,
    mills: std::path::PathBuf,
}
impl Modules {
    pub fn new(root: &Path, cfg: &Config) -> Result<Modules, String> {
        let front_secret = std::env::var("VR_FRONT_SECRET").ok().filter(|s| s.len() >= 32).unwrap_or_else(|| vr_core::crypto::random_hex(24));
        let mills = root.join("..");
        Ok(Modules { front_secret, auth: auth::Auth::new(root, cfg), board: Arc::new(board::Board::new(mills.join("Board"))), mirror: Arc::new(forgejo::Mirror::new(root.to_path_buf(), cfg)), notes: notes::Notes::new(mills.join("Notes")),
            files: files::Files::new(&mills, root, cfg), mills })
    }
    /// Whether someone has set up their chat key (Chat/keys.json, the chat's: still the JavaScript hub's).
    fn has_chat_key(&self, u: &str) -> bool { std::fs::read_to_string(self.mills.join("Chat").join("keys.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| !v[u].is_null()).unwrap_or(false) }
    /// How a person is named on the board and in notes: their nickname, else their display name (bridge/server.js nameOf).
    pub fn name_of(&self, u: &str) -> String {
        let Some(a) = self.auth.account(u) else { return u.to_string() };
        std::fs::read_to_string(a.dir.join("state.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|s| s["prefs"]["nick"].as_str().filter(|n| !n.is_empty()).map(String::from)).unwrap_or(a.display)
    }
    pub fn has(&self, u: &str) -> bool { self.auth.account(u).is_some() }
    pub fn board_op(&self, op: &str, a: &board::Actor, b: &Value) -> board::Out {
        let out = self.board.op(op, a, b, &|u| self.has(u), &|u| self.name_of(u));
        self.mirror.changed(&self.board, out.changes.clone());
        out
    }
    fn actor(&self, user: &str, agent: bool) -> board::Actor { board::Actor { user: user.to_string(), agent, name: self.name_of(user) } }
}
pub fn start_background(hub: &Arc<Hub>) {
    // the Forgejo → board poll
    if let Some(every) = hub.mods.mirror.every() {
        let h = hub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            loop {
                let h2 = h.clone();
                let ops = move |op: &str, b: &Value| -> Value { let fa = board::Actor { user: "forgejo".into(), agent: false, name: "Forgejo".into() }; h2.mods.board_op(op, &fa, b).body };
                h.mods.mirror.poll(&h.mods.board, &ops).await;
                tokio::time::sleep(every).await;
            }
        });
    }
    // files: the hourly sweep (chat uploads never sent, the old Work layout)
    let hf = hub.clone();
    tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(3600)).await; let accts = hf.mods.auth.accounts(); hf.mods.files.chat_sweep(&|u| hf.mods.auth.account(u)); hf.mods.files.work_sweep(&accts); } });
    // the notes changed: pages hear it through the chat's long poll (the JavaScript hub's)
    let h = hub.clone();
    tokio::spawn(async move {
        let mut rx = h.mods.notes.ver.subscribe();
        while rx.changed().await.is_ok() { crate::front::internal(&h, "/internal/front/room-changed", "{}".into()).await; }
    });
}
fn from_front(hub: &Hub, r: &Req) -> bool {
    use subtle::ConstantTimeEq;
    r.peer.ip().is_loopback() && r.header("x-vr-front-secret").map(|s| bool::from(s.as_bytes().ct_eq(hub.mods.front_secret.as_bytes()))).unwrap_or(false)
}
fn answer(status: u16, v: Value) -> Routed { Routed::Answer(jresp(status, &v)) }

/// The JavaScript hub's own "moving house" switch (bridge/.maintenance): its text is the new address.
fn maintenance(hub: &Hub) -> Option<String> { std::fs::read_to_string(hub.root.join(".maintenance")).ok().map(|t| { let t = t.trim().to_string(); if t.is_empty() { "the server".into() } else { t } }) }

pub async fn route(hub: &Arc<Hub>, r: &Req) -> Routed {
    let p = r.path.as_str();
    // the front itself: who answers here (for the tools and the regression suite; nothing private)
    if p == "/__vr-front" { return Routed::Answer(jresp(200, &json!({ "front": "vr-hub", "version": crate::VERSION, "modules": native_modules() }))); }
    // the JavaScript hub asking a module of this one (with the front's secret, from this machine only)
    if let Some(rest) = p.strip_prefix("/__front/") {
        if !from_front(hub, r) { return answer(403, json!({ "ok": false, "error": "forbidden" })); }
        let b = r.json();
        let account = b["account"].as_str().unwrap_or("").to_string();
        return match rest {
            "board/tool" => {
                let Some(op) = board::tool_op(b["name"].as_str().unwrap_or("")) else { return answer(200, json!({ "ok": false, "error": format!("Unknown board tool: {}", b["name"].as_str().unwrap_or("")) })) };
                if !hub.mods.has(&account) { return answer(200, json!({ "ok": false, "error": "Unknown account." })); }
                let out = hub.mods.board_op(op, &hub.mods.actor(&account, true), &if b["args"].is_object() { b["args"].clone() } else { json!({}) });
                answer(200, if out.status == 200 { json!({ "ok": true, "result": out.body }) } else { json!({ "ok": false, "error": out.body["error"] }) })
            }
            "notes/tool" => answer(200, hub.mods.notes.tool(b["name"].as_str().unwrap_or(""), &b["args"], &hub.mods.name_of(&account))),
            // the chat (the JavaScript hub's) about its messages' files
            "files/attach" => {
                let user = b["user"].as_str().unwrap_or("");
                match hub.mods.files.attach(hub.mods.auth.account(user).as_ref(), user, &b["ids"], b["seq"].as_i64().unwrap_or(0), b["conv"].as_str().unwrap_or("")) {
                    Ok(ids) => answer(200, json!({ "ok": true, "ids": ids })),
                    Err(f) => answer(200, json!({ "ok": false, "status": f.status, "error": f.msg })),
                }
            }
            "files/gone" => { let ids: Vec<String> = b["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(); hub.mods.files.message_gone(&ids, &|u| hub.mods.auth.account(u)); answer(200, json!({ "ok": true })) }
            "files/all-gone" => { hub.mods.files.all_gone(&|u| hub.mods.auth.account(u)); answer(200, json!({ "ok": true })) }
            "files/work-message" => match hub.mods.auth.account(&account) {
                Some(a) => answer(200, hub.mods.files.work_message(&a, b["message"].as_str().unwrap_or(""), b["where"].clone())),
                None => answer(200, json!({ "text": b["message"], "files": [], "dirs": [] })),
            },
            _ => answer(404, json!({ "ok": false, "error": "unknown" })),
        };
    }
    // never trust these from outside: only the front sets them
    // (the proxy strips any x-vr-front-* / x-vr-user header a caller sent)
    if !p.starts_with("/api/") { return Routed::Pass(vec![]); }
    // moving house (bridge/handoff.js pack writes bridge/.maintenance): every change answers "moved", reading and the
    // Work run doing the move go on (the JavaScript hub's rule, for everything answered here and there)
    if r.method == hyper::Method::POST && !matches!(p, "/api/chat" | "/api/stop" | "/api/pc-result") && p != "/api/status" {
        if let Some(w) = maintenance(hub) { return answer(503, json!({ "error": format!("VibeRoller has moved to {w}: open it there and sign in as usual. This copy is read-only now.") })); }
    }
    let a = &hub.mods.auth;
    if p == "/api/login" && r.method == hyper::Method::POST { return a.login(r, maintenance(hub)).await; }
    let user = a.session_user(r);
    // what the JavaScript hub learns with every request: who it is, and the notes' version (its chat poll carries it)
    let me = || { let mut h = vec![("x-vr-notes-ver".to_string(), hub.mods.notes.version().to_string())]; if let Some(u) = &user { h.push(("x-vr-user".to_string(), u.clone())); } h };
    // ----- the board and the notes: this hub's own now -----
    if p == "/api/board" || p.starts_with("/api/board/") || p == "/api/notes" || p.starts_with("/api/notes/") {
        let Some(u) = user.clone() else { return answer(401, json!({ "error": "Not signed in." })) };
        if p.starts_with("/api/board") { return board_http(hub, r, &u).await; }
        return notes_http(hub, r, &u);
    }
    // ----- files: the chat's attachments (ciphertext), Work uploads, the Files library -----
    if p == "/api/room/file" || p.starts_with("/api/room/file/") || p == "/api/work/file" || p.starts_with("/api/work/file/") || p == "/api/library" || p.starts_with("/api/library/") {
        let Some(u) = user.clone() else { return answer(401, json!({ "error": "Not signed in." })) };
        return files_http(hub, r, &u).await;
    }
    // the admin's account switches that are this module's (the JavaScript hub keeps making / deleting accounts)
    if r.method == hyper::Method::POST && matches!(p, "/api/admin/password" | "/api/admin/unlock" | "/api/admin/signout") {
        let Some(u) = &user else { return Routed::Answer(jresp(401, &json!({ "error": "Not signed in." }))) };
        if !a.account(u).map(|x| x.admin).unwrap_or(false) { return Routed::Answer(jresp(403, &json!({ "error": "Admins only." }))); }
        let target = r.json()["username"].as_str().and_then(|t| a.account(t));
        let Some(t) = target else { return Routed::Answer(jresp(400, &json!({ "error": "No such account." }))) };
        return Routed::Answer(match p {
            "/api/admin/password" => {
                let pw = auth::new_password();
                let f = t.dir.join("auth.json");
                let tmp = f.with_extension("json.tmp");
                if std::fs::write(&tmp, serde_json::to_string_pretty(&auth::hash_password(&pw)).unwrap_or_default()).and_then(|_| std::fs::rename(&tmp, &f)).is_err() { return Routed::Answer(jresp(500, &json!({ "error": "Could not save the password." }))); }
                if t.username != *u { a.drop_sessions(&t.username); }
                println!("[vr-hub] {u} set a new password for \"{}\".", t.username);
                jresp(200, &json!({ "username": t.username, "password": pw }))
            }
            "/api/admin/unlock" => { a.unlock(&t.username); jresp(200, &json!({ "ok": true })) }
            _ => { if t.username != *u { a.drop_sessions(&t.username); } jresp(200, &json!({ "ok": true })) }
        });
    }
    Routed::Pass(me())
}

async fn board_http(hub: &Arc<Hub>, r: &Req, u: &str) -> Routed {
    let m = &hub.mods;
    if r.path == "/api/board" && r.method == hyper::Method::GET { // a long poll like the chat: ?wait=1&ver=N
        m.board.seen(u);
        if r.param("wait").as_deref() == Some("1") && r.param("ver").and_then(|v| v.parse::<u64>().ok()) == Some(m.board.version()) {
            let mut rx = m.board.ver.subscribe();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(25), rx.changed()).await;
            m.board.seen(u);
        }
        return answer(200, m.board.view(u, &|x| m.name_of(x), &|x| m.has(x), m.mirror.status()));
    }
    let Some(op) = board::route_op(&r.path) else { return answer(404, json!({ "error": "Unknown endpoint" })) };
    let b = if r.method == hyper::Method::POST { if r.body.is_empty() { json!({}) } else { match serde_json::from_slice::<Value>(&r.body) { Ok(v) => v, Err(_) => return answer(400, json!({ "error": "That request is not valid JSON." })) } } }
        else { Value::Object(crate::front::url_params(&r.query).into_iter().map(|(k, v)| (k, json!(v))).collect()) };
    let out = m.board_op(op, &m.actor(u, false), &b);
    answer(out.status, out.body)
}
fn notes_http(hub: &Arc<Hub>, r: &Req, u: &str) -> Routed {
    let n = &hub.mods.notes;
    let res = |x: Result<Value, notes::Err>| match x { Ok(v) => answer(200, v), Err(e) => answer(e.status, json!({ "error": e.msg })) };
    match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/notes") => answer(200, json!({ "ver": n.version(), "tree": n.tree(&[]) })),
        ("GET", "/api/notes/read") => res(n.read(&r.param("path").unwrap_or_default())),
        ("GET", "/api/notes/search") => answer(200, json!({ "items": n.search(&r.param("q").unwrap_or_default()) })),
        ("POST", p) => {
            let b = r.json(); let who = hub.mods.name_of(u);
            let s = |k: &str| b[k].as_str().map(String::from).unwrap_or_else(|| if b[k].is_null() { String::new() } else { b[k].to_string() });
            match p {
                "/api/notes/write" => res(n.write(&who, &s("path"), &s("text"), b["baseAt"].as_f64())),
                "/api/notes/delete" => res(n.remove(&s("path"))),
                "/api/notes/move" => res(n.mv(&who, &s("from"), &s("to"))),
                "/api/notes/folder" => res(n.folder(&s("path"))),
                _ => answer(404, json!({ "error": "Unknown endpoint" })),
            }
        }
        _ => answer(404, json!({ "error": "Unknown endpoint" })),
    }
}

/// A file handed back: never as a type a browser would run (octet-stream + nosniff + attachment; the page shows
/// pictures / video from a blob of the type it knows, named in X-VR-File-Type).
fn serve_file(buf: Vec<u8>, name: &str, mime: &str) -> Routed {
    let resp = hyper::Response::builder().status(200).header("content-type", "application/octet-stream").header("content-length", buf.len())
        .header("content-disposition", format!("attachment; filename=\"{}\"", vr_core::hub::urlencode(name))).header("x-vr-file-type", mime).header("cache-control", "private, no-store").header("x-content-type-options", "nosniff")
        .body(crate::front::full(buf)).expect("response");
    Routed::Answer(resp)
}
async fn files_http(hub: &Arc<Hub>, r: &Req, u: &str) -> Routed {
    let m = &hub.mods;
    let Some(a) = m.auth.account(u) else { return answer(401, json!({ "error": "Not signed in." })) };
    let fail = |f: files::Fail| answer(f.status, json!({ "error": f.msg }));
    let get = r.method == hyper::Method::GET; let post = r.method == hyper::Method::POST;
    match r.path.as_str() {
        "/api/room/file" if post => {
            if !m.has_chat_key(u) { return answer(400, json!({ "error": "Your chat key is not set up yet." })); }
            match m.files.chat_upload(&a, &r.body, r.header("x-vr-own-seal").unwrap_or("")) { Ok(v) => answer(200, v), Err(f) => fail(f) }
        }
        p if get && p.starts_with("/api/room/file/") => {
            let id = &p["/api/room/file/".len()..];
            let Some((by, seq)) = m.files.owner_seq(id) else { return answer(404, json!({ "error": "Not found." })) };
            // its uploader always; anyone else while they may read its message: the chat (the JavaScript hub) knows
            let allowed = by == u || (seq > 0 && crate::front::internal(hub, "/internal/front/may-read", json!({ "user": u, "seq": seq }).to_string()).await.and_then(|(_, b)| serde_json::from_slice::<Value>(&b).ok()).map(|v| v["ok"] == json!(true)).unwrap_or(false));
            match m.files.fetch(id, m.auth.account(&by), allowed) { Some(buf) => serve_file(buf, "file", "application/octet-stream"), None => answer(404, json!({ "error": "Not found." })) }
        }
        "/api/work/file" if post => match m.files.work_upload(&a, &r.body, r.header("x-file-name").unwrap_or(""), r.header("content-type").unwrap_or("")) { Ok(v) => answer(200, v), Err(f) => fail(f) },
        p if get && p.starts_with("/api/work/file/") => match m.files.work_serve(&a, &p["/api/work/file/".len()..]) { Some((buf, name, mime)) => serve_file(buf, &name, &mime), None => answer(404, json!({ "error": "That file is no longer kept." })) },
        "/api/library" if get => answer(200, json!({ "linux": m.files.linux.status(u, &files::Files::fallback_dir(&a)), "entries": m.files.list(&a) })),
        "/api/library/delete" if post => if m.files.remove(&a, r.json()["id"].as_str().unwrap_or("")) { answer(200, json!({ "ok": true })) } else { answer(404, json!({ "error": "Not found." })) },
        "/api/library/save" if post => match m.files.save(&a, &r.param("id").unwrap_or_default(), &r.body) { Ok(v) => answer(200, v), Err(f) => fail(f) },
        "/api/library/exec" if post => { let b = r.json(); match m.files.set_exec(&a, b["id"].as_str().unwrap_or(""), b["on"].as_bool().unwrap_or(false)) { Ok(v) => answer(200, v), Err(f) => fail(f) } }
        _ => answer(404, json!({ "error": "Unknown endpoint" })),
    }
}

/// After the JavaScript hub answered: the module's part of a few answers (sign-out drops the session; the admin list
/// gets the locks and sign-ins this module keeps; a deleted account loses its sessions and lock).
pub fn after(hub: &Hub, r: &Req, status: u16, body: Option<&[u8]>) -> Option<Vec<u8>> {
    let a = &hub.mods.auth;
    match r.path.as_str() {
        "/api/logout" if status == 200 => {
            if let Some(t) = r.header("authorization").and_then(|v| v.strip_prefix("Bearer ")) { a.forget_session(&auth::token_key(t)); }
            None
        }
        "/api/admin/delete" if status == 200 => { if let Some(u) = r.json()["username"].as_str() { a.drop_sessions(u); a.unlock(u); } None }
        "/api/admin/accounts" if status == 200 => {
            let mut v: Value = serde_json::from_slice(body?).ok()?;
            for acc in v["accounts"].as_array_mut()? {
                let u = acc["username"].as_str().unwrap_or("").to_string();
                let lock = a.lock_of(&u);
                acc["locked"] = json!(lock.is_some());
                acc["lockedAt"] = lock.map(|l| l["at"].clone()).unwrap_or(Value::Null);
                acc["signedIn"] = json!(a.signed_in(&u));
            }
            Some(v.to_string().into_bytes())
        }
        _ => None,
    }
}
/// Which answers `after` needs to see whole (the rest stream through untouched).
pub fn wants_after(r: &Req) -> bool { matches!(r.path.as_str(), "/api/logout" | "/api/admin/delete" | "/api/admin/accounts") }

/// The modules this front answers itself.
pub fn native_modules() -> Vec<&'static str> { vec!["auth", "board", "notes", "files"] }
