//! The hub's modules that run here in Rust. Each one owns its routes and its data files outright (the JavaScript hub no
//! longer touches them); where the JavaScript hub still needs one of them, it asks it over the internal API
//! (/internal/front/…, with the front's secret), and where one of them needs the JavaScript hub, it asks it the same way.
//! The order they move in: board T-131. Moved so far: accounts & authentication (auth.rs).
use hyper::Response;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::Arc;

use crate::front::{json as jresp, Body, Hub, Req};

pub mod ai_hosts;
pub mod auth;
pub mod board;
pub mod chat;
pub mod files;
pub mod forgejo;
pub mod guilds;
pub mod perms;
pub mod native_releases;
pub mod notes;
pub mod openai;
pub mod unity;
pub mod unity_brain;
pub mod unity_routes;
pub mod unity_time;
pub mod unity_web;
pub mod usage;
pub mod work;
pub mod worktree;

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
    #[allow(dead_code)]
    No,
}

pub struct Modules {
    pub front_secret: String,
    pub auth: auth::Auth,
    pub board: Arc<board::Board>,
    pub mirror: Arc<forgejo::Mirror>,
    pub notes: notes::Notes,
    pub files: files::Files,
    pub ai: Arc<ai_hosts::AiHosts>,
    pub native: native_releases::Releases,
    pub work: Arc<work::Work>,
    pub api: openai::Api,
    pub chat: Arc<chat::Chat>,
    pub unity: unity::Unity,
    pub unity_room: unity_routes::RoomState,
    mills: std::path::PathBuf,
}
impl Modules {
    pub fn new(root: &Path, cfg: &Config) -> Result<Modules, String> {
        let front_secret = std::env::var("VR_FRONT_SECRET").ok().filter(|s| s.len() >= 32).unwrap_or_else(|| vr_core::crypto::random_hex(24));
        let mills = root.join("..");
        let usage = Arc::new(usage::Usage::new(root, cfg, true));
        let auth = auth::Auth::new(root, cfg);
        let accounts = auth.accounts();
        let api = openai::Api::new(root, &accounts);
        let users: Vec<(String, String, bool)> = accounts.into_iter().map(|a| (a.username, a.display, a.admin)).collect();
        let chat = Arc::new(chat::Chat::new(&mills, root, cfg, &users));
        Ok(Modules { front_secret, auth, api, chat, unity: unity::Unity::new(root, cfg), unity_room: Default::default(), board: Arc::new(board::Board::new(mills.join("Board"))), mirror: Arc::new(forgejo::Mirror::new(root.to_path_buf(), cfg)), notes: notes::Notes::new(mills.join("Notes")),
            files: files::Files::new(&mills, root, cfg), ai: ai_hosts::AiHosts::new(root, cfg, usage), native: native_releases::Releases::new(root), work: Arc::new(work::Work::new(root, cfg)), mills })
    }
    pub fn mills_dir(&self) -> std::path::PathBuf { self.mills.clone() }
    /// config publicUrl when it is an https address, else this machine (bridge/server.js PUBLIC_URL)
    fn public_url(&self, cfg: &Config) -> String {
        let u = cfg.get("publicUrl").and_then(Value::as_str).unwrap_or("").trim_end_matches('/').to_lowercase();
        let host = u.strip_prefix("https://").unwrap_or("");
        let (name, port) = host.split_once(':').unwrap_or((host, ""));
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') && port.bytes().all(|b| b.is_ascii_digit()) && (port.is_empty() == !host.contains(':')) { u } else { format!("http://127.0.0.1:{}", cfg.port) }
    }
    /// Whether someone has set up their chat key (Chat/keys.json, the chat's: still the JavaScript hub's).
    fn has_chat_key(&self, u: &str) -> bool { self.chat.has_key(u) }
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
    // AI hosts: the up / down watch, what changed to the JavaScript hub (its snapshot), each host window to its usage
    hub.mods.ai.watch();
    let h = hub.clone();
    tokio::spawn(async move { loop { h.mods.ai.dirty.notified().await; tokio::time::sleep(std::time::Duration::from_millis(50)).await; push_ai_state(&h).await; } });
    work::start(hub);
    chat::start(hub);
    unity_brain::start(hub);
    // the notes changed: pages hear it through the chat's long poll (the JavaScript hub's)
    let h = hub.clone();
    tokio::spawn(async move {
        let mut rx = h.mods.notes.ver.subscribe();
        while rx.changed().await.is_ok() { h.mods.chat.room_changed(); }
    });
}
/// The AI hosts' state to the JavaScript hub (see AiHosts::snapshot).
pub async fn push_ai_state(hub: &Hub) {
    let users: Vec<String> = hub.mods.auth.accounts().into_iter().map(|a| a.username).collect();
    let snap = hub.mods.ai.snapshot(&users);
    crate::front::internal(hub, "/internal/front/hosts-state", snap.to_string()).await;
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
            p if p.starts_with("ai/") => ai_front(hub, r, &p[3..]).await,
            // its Unity (post, what she was sent, composing), its profiles changed
            p if p.starts_with("chat/") => answer(200, chat::front(hub, &p[5..], &b)),
            _ => answer(404, json!({ "ok": false, "error": "unknown" })),
        };
    }
    // ----- AI hosts: their own id + secret, no browser session -----
    if let Some(rest) = p.strip_prefix("/node/job/") {
        // the MCP tools of a Work run on a host, with the job's own token, as that person
        let (id, tool) = rest.split_once("/internal/").unwrap_or(("", ""));
        if id.is_empty() || !matches!(tool, "pc-tool" | "board" | "team") { return answer(404, json!({ "error": "Not found" })); }
        return match hub.mods.ai.job_for(id, r.header("x-mills-internal").unwrap_or("")) {
            Some((account, _)) => answer(200, work::tool(hub, if tool == "pc-tool" { "pc" } else { tool }, &account, &r.json()).await),
            None => answer(403, json!({ "ok": false, "error": "forbidden" })),
        };
    }
    // the MCP tools of a Work run here (this hub's own secret; anything else is the JavaScript hub's)
    if let Some(tool) = p.strip_prefix("/internal/").filter(|t| matches!(*t, "pc-tool" | "board" | "team")) {
        if r.method == hyper::Method::POST && work::same_secret(r.header("x-mills-internal").unwrap_or(""), &hub.mods.work.secret) {
            let account = r.header("x-mills-account").filter(|a| !a.is_empty()).map(String::from).unwrap_or_else(|| hub.cfg.get("defaultUsername").and_then(Value::as_str).unwrap_or("mills").to_string());
            return answer(200, work::tool(hub, if tool == "pc-tool" { "pc" } else { tool }, &account, &r.json()).await);
        }
        return Routed::Pass(vec![]);
    }
    if p.starts_with("/llm/") { return work::llm(hub, r).await; }
    // Unity's Claude brain calling her tools (this hub's secret; anything else is the JavaScript hub's)
    if p == "/internal/assistant-tool" && work::same_secret(r.header("x-mills-internal").unwrap_or(""), &hub.mods.unity.secret) { return unity_routes::internal(hub, r).await; }
    // the OpenAI- / Anthropic-compatible API: its own bearer keys, for harnesses, not the browser app
    if p == "/v1" || p.starts_with("/v1/") { return openai::v1(hub, r).await; }
    if p.starts_with("/node/") {
        let (status, out) = hub.mods.ai.route(&r.method, p, &r.query, &r.headers, &r.body, &hub.mods.native).await;
        // a host that said hello is on everyone's list before it hears back
        if p == "/node/hello" && status == 200 { push_ai_state(hub).await; }
        return Routed::Answer(match out {
            Ok(v) => jresp(status, &v),
            Err((buf, mime)) => hyper::Response::builder().status(status).header("content-type", mime).header("content-length", buf.len()).header("cache-control", "no-store").body(crate::front::full(buf)).expect("response"),
        });
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
    // ----- Work: runs, model / provider, project, usage, shared sessions, the conversation + queue, past sessions -----
    if let Some(u) = &user {
        if let Some(acc) = a.account(u) {
            if let Some(x) = work::route(hub, r, &acc).await { return x; }
            if let Some(x) = openai::route(hub, r, &acc) { return x; } // Settings → API access, → AI providers, → My AI hosts' list
            if let Some(x) = chat::route(hub, r, &acc).await { return x; } // the chat, servers, calls, presence
            if let Some(x) = unity_routes::route(hub, r, &acc).await { return x; } // Unity's section
            if p == "/api/admin/unity-tools" && r.method == hyper::Method::POST { // the admin switches Unity's own-computer tools for one account
                if !acc.admin { return answer(403, json!({ "error": "Admins only." })); }
                let b = r.json();
                let Some(t) = b["username"].as_str().and_then(|x| a.account(x)) else { return answer(400, json!({ "error": "No such account." })) };
                let u = &hub.mods.unity;
                u.set_state(&t, |m| { if let Some(x) = b["pc"].as_bool() { m.insert("pc".into(), json!(x)); } if let Some(x) = b["code"].as_bool() { m.insert("code".into(), json!(x)); } });
                return answer(200, json!({ "username": t.username, "pc": u.group_on(&t, "pc"), "code": u.group_on(&t, "code") }));
            }
        }
    }
    // ----- Settings → My AI hosts (the list itself, GET /api/hosts, is the JavaScript hub's: it adds the default provider) -----
    if r.method == hyper::Method::POST && matches!(p, "/api/hosts/new" | "/api/hosts/rename" | "/api/hosts/remove" | "/api/hosts/share" | "/api/hosts/config") {
        let Some(u) = user.clone() else { return answer(401, json!({ "error": "Not signed in." })) };
        let (ai, b) = (&hub.mods.ai, r.json());
        let s = |k: &str| b[k].as_str().map(String::from).unwrap_or_else(|| if b[k].is_null() { String::new() } else { b[k].to_string() });
        let out = match p {
            "/api/hosts/new" => ai.create(&u, &s("nickname")).map(|h| json!({ "host": h, "mine": ai.mine(&u) })),
            "/api/hosts/rename" => ai.rename(&u, &s("id"), &s("nickname")).map(|h| json!({ "host": h })),
            "/api/hosts/remove" => ai.remove(&u, &s("id")).map(|_| json!({ "mine": ai.mine(&u) })),
            "/api/hosts/share" => ai.set_share(&u, &s("id"), &s("provider"), &if b["users"].is_null() && b.get("users").is_some() { Value::Null } else if b["users"].is_array() { b["users"].clone() } else { json!([]) }).map(|h| json!({ "host": h })),
            _ => {
                let models: Vec<String> = hub.cfg.get("models").and_then(Value::as_array).map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default();
                ai.config_for(&u, &s("id"), &hub.mods.public_url(&hub.cfg), models).map(|c| json!({ "config": c }))
            }
        };
        if out.is_ok() { push_ai_state(hub).await; }
        return match out { Ok(v) => answer(200, v), Err(e) => answer(400, json!({ "error": e })) };
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

/// A body that comes piece by piece (a host's answer as it streams, a Work run's events).
fn stream_body(rx: tokio::sync::mpsc::Receiver<Result<bytes::Bytes, String>>) -> Body {
    use http_body_util::BodyExt;
    let s = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x.map(hyper::body::Frame::data).map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e)), rx)) });
    http_body_util::StreamBody::new(s).boxed()
}
/// The JavaScript hub's Work runs, API, chat and Unity using the AI hosts (/__front/ai/…).
async fn ai_front(hub: &Arc<Hub>, r: &Req, what: &str) -> Routed {
    let ai = &hub.mods.ai;
    match what {
        "state" => { let users: Vec<String> = hub.mods.auth.accounts().into_iter().map(|a| a.username).collect(); answer(200, ai.snapshot(&users)) }
        // a call carried to a host: engine (Unity's), provider (a host's endpoint) or login (a host's Claude / Codex login)
        "fetch" => {
            use base64::Engine;
            let meta: Value = r.header("x-vr-ai").and_then(|h| base64::engine::general_purpose::STANDARD.decode(h).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
            let s = |k: &str| meta[k].as_str().unwrap_or("").to_string();
            let headers: Vec<(String, String)> = meta["headers"].as_object().map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect()).unwrap_or_default();
            let method = if s("method").is_empty() { "GET".to_string() } else { s("method") };
            let body = r.body.to_vec();
            let res = match s("mode").as_str() {
                "engine" => ai.engine_fetch(&s("engine"), &s("path"), &method, headers, body).await,
                "provider" => ai.provider_fetch(&s("host"), &s("provider"), &s("path"), &method, headers, body).await,
                "login" => ai.login_fetch(&s("host"), &s("provider"), &s("path"), &method, headers, body).await,
                _ => Err(ai_hosts::HostErr { status: 400, msg: "unknown mode".into() }),
            };
            match res {
                Ok(rel) => {
                    // a login's usage headers may have moved its caps: the JavaScript hub knows before the answer ends
                    if rel.headers.iter().any(|(k, _)| k.starts_with("anthropic-ratelimit-unified-")) { push_ai_state(hub).await; }
                    let mut b = hyper::Response::builder().status(rel.status).header("cache-control", "no-store");
                    for (k, v) in &rel.headers { b = b.header(k.as_str(), v.as_str()); }
                    Routed::Answer(b.body(stream_body(rel.body)).expect("response"))
                }
                Err(e) => { let mut resp = jresp(e.status, &json!({ "error": e.msg })); resp.headers_mut().insert("x-vr-ai-error", hyper::header::HeaderValue::from_static("1")); Routed::Answer(resp) }
            }
        }
        // a Work run on a host: one JSON line per event ({ id } first, { ev } …, then { done } or { error })
        "work" => {
            let b = r.json();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, String>>(256);
            let line = |v: Value| Ok(bytes::Bytes::from(v.to_string() + "\n"));
            match ai.run_work(b["host"].as_str().unwrap_or(""), b["provider"].as_str().unwrap_or(""), b["payload"].clone()) {
                Err(e) => { let _ = tx.try_send(line(json!({ "error": e.msg, "status": e.status }))); }
                Ok((id, _token, mut events)) => {
                    let _ = tx.try_send(line(json!({ "id": id })));
                    let ai = ai.clone();
                    tokio::spawn(async move {
                        while let Some(m) = events.recv().await {
                            let (v, end) = match m { ai_hosts::WorkMsg::Event(ev) => (json!({ "ev": ev }), false), ai_hosts::WorkMsg::Done(c) => (json!({ "done": c }), true), ai_hosts::WorkMsg::Error(e) => (json!({ "error": e }), true) };
                            // the JavaScript hub hung up on the run: the host is told to stop it
                            if tx.send(line(v)).await.is_err() { ai.cancel_work(&id); return; }
                            if end { return; }
                        }
                    });
                }
            }
            Routed::Answer(hyper::Response::builder().status(200).header("content-type", "application/x-ndjson").header("cache-control", "no-store").body(stream_body(rx)).expect("response"))
        }
        "cancel" => { ai.cancel_work(r.json()["id"].as_str().unwrap_or("")); answer(200, json!({ "ok": true })) }
        "pc-tool" => {
            let b = r.json();
            match ai.pc_tool(b["host"].as_str().unwrap_or(""), b["name"].as_str().unwrap_or(""), &if b["args"].is_object() { b["args"].clone() } else { json!({}) }).await {
                Ok(v) => answer(200, json!({ "result": v })),
                Err(e) => answer(200, json!({ "error": e.msg })),
            }
        }
        _ => answer(404, json!({ "ok": false, "error": "unknown" })),
    }
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
            let allowed = by == u || (seq > 0 && chat::may_read(hub, u, seq));
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
    let a = &hub.mods.auth; let w = &hub.mods.work;
    let target = || r.json()["username"].as_str().map(String::from);
    match r.path.as_str() {
        "/api/logout" if status == 200 => {
            if let Some(u) = a.session_user(r) { w.share_stop(&u); chat::calls_leave(hub, &u); hub.mods.chat.set_typing(&u, "", false); hub.mods.chat.gone(&u); }
            if let Some(t) = r.header("authorization").and_then(|v| v.strip_prefix("Bearer ")) { a.forget_session(&auth::token_key(t)); }
            None
        }
        "/api/admin/delete" if status == 200 => {
            if let Some(u) = target() {
                a.drop_sessions(&u); a.unlock(&u); w.stop(hub, &u); w.share_stop(&u); w.forget(&u);
                let c = &hub.mods.chat; c.drop_key(&u); chat::calls_leave(hub, &u);
                let us: Vec<(String, String, bool)> = a.accounts().into_iter().map(|x| (x.username, x.display, x.admin)).collect();
                let ctx = guilds::Ctx { users: &us, seq: c.seq(), msg_conv: &|n| c.msg_conv(n), delete_recent: &|_, _, _| 0 };
                c.guilds.drop_user(&u, &ctx); c.keys_changed();
            }
            None
        }
        "/api/admin/stop" if status == 200 => { if let Some(u) = target() { w.stop(hub, &u); hub.mods.api.stop(&u); hub.mods.unity.stop(&u); } None }
        "/api/projects" if status == 200 => { w.projects_changed(); None }
        "/api/admin/accounts" if status == 200 => {
            let mut v: Value = serde_json::from_slice(body?).ok()?;
            for acc in v["accounts"].as_array_mut()? {
                let u = acc["username"].as_str().unwrap_or("").to_string();
                let lock = a.lock_of(&u);
                acc["locked"] = json!(lock.is_some());
                acc["lockedAt"] = lock.map(|l| l["at"].clone()).unwrap_or(Value::Null);
                acc["signedIn"] = json!(a.signed_in(&u));
                acc["busy"] = json!(acc["busy"] == json!(true) || w.busy(&u));
                acc["chatKey"] = json!(hub.mods.chat.has_key(&u));
                if let Some(x) = a.account(&u) { acc["unityPc"] = json!(hub.mods.unity.group_on(&x, "pc")); acc["unityCode"] = json!(hub.mods.unity.group_on(&x, "code")); acc["busy"] = json!(acc["busy"] == json!(true) || hub.mods.unity.runs.lock().unwrap().contains_key(&u)); }
                if let Some(x) = a.account(&u) { acc["model"] = json!(w.effective_model(&x)); }
            }
            Some(v.to_string().into_bytes())
        }
        // this hub's own part of what a page learns at start: the Work choices, the project, the share
        "/api/check" if status == 200 => {
            let mut v: Value = serde_json::from_slice(body?).ok()?;
            let acc = a.session_user(r).and_then(|u| a.account(&u))?;
            let st = w.state(&acc);
            v["model"] = json!(s_of(&st, "model"));
            v["provider"] = json!(if s_of(&st, "provider").is_empty() { "host".to_string() } else { s_of(&st, "provider") });
            v["effectiveProvider"] = json!(w.effective(&hub.mods.ai, &acc.username, &s_of(&st, "provider")).map(|p| s_of(&p, "id")).unwrap_or_default());
            v["project"] = json!(w.project_of(&acc)); v["projects"] = w.project_list();
            v["share"] = json!(w.share_of(&acc.username));
            v["providers"] = json!(work::providers_list(hub, &acc.username));
            v["defaultProvider"] = json!(w.default_for(&hub.mods.ai, &acc.username).map(|p| s_of(&p, "id")).unwrap_or_default());
            v["hubClaude"] = json!(hub.cfg.get("hubClaude") != Some(&json!(false)));
            Some(v.to_string().into_bytes())
        }
        // busy counts this hub's Work runs too (restart.js and the regression runner wait for nobody being busy)
        "/api/status" if status == 200 => {
            let mut v: Value = serde_json::from_slice(body?).ok()?;
            if v.get("busy").is_some() && (w.any_busy() || hub.mods.api.busy() || hub.mods.unity.busy() || unity_routes::room_busy(hub)) { v["busy"] = json!(true); return Some(v.to_string().into_bytes()); }
            None
        }
        _ => None,
    }
}
fn s_of(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_string() }
/// Which answers `after` needs to see whole (the rest stream through untouched).
pub fn wants_after(r: &Req) -> bool { matches!(r.path.as_str(), "/api/logout" | "/api/admin/delete" | "/api/admin/accounts" | "/api/admin/stop" | "/api/check" | "/api/status") || (r.path == "/api/projects" && r.method == hyper::Method::POST) }

/// The modules this front answers itself.
pub fn native_modules() -> Vec<&'static str> { vec!["auth", "board", "notes", "files", "aihosts", "work", "api", "chat", "unity"] }
