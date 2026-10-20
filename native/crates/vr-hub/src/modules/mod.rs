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

pub struct Modules { pub front_secret: String, pub auth: auth::Auth }
impl Modules {
    pub fn new(root: &Path, cfg: &Config) -> Result<Modules, String> {
        let front_secret = std::env::var("VR_FRONT_SECRET").ok().filter(|s| s.len() >= 32).unwrap_or_else(|| vr_core::crypto::random_hex(24));
        Ok(Modules { front_secret, auth: auth::Auth::new(root, cfg) })
    }
}
pub fn start_background(_hub: &Arc<Hub>) {}

/// The JavaScript hub's own "moving house" switch (bridge/.maintenance): its text is the new address.
fn maintenance(hub: &Hub) -> Option<String> { std::fs::read_to_string(hub.root.join(".maintenance")).ok().map(|t| { let t = t.trim().to_string(); if t.is_empty() { "the server".into() } else { t } }) }

pub async fn route(hub: &Arc<Hub>, r: &Req) -> Routed {
    let p = r.path.as_str();
    // the front itself: who answers here (for the tools and the regression suite; nothing private)
    if p == "/__vr-front" { return Routed::Answer(jresp(200, &json!({ "front": "vr-hub", "version": crate::VERSION, "modules": native_modules() }))); }
    // never trust these from outside: only the front sets them
    // (the proxy strips any x-vr-front-* header a caller sent; x-vr-user is replaced below)
    if !p.starts_with("/api/") { return Routed::Pass(vec![]); }
    let a = &hub.mods.auth;
    if p == "/api/login" && r.method == hyper::Method::POST { return a.login(r, maintenance(hub)).await; }
    let user = a.session_user(r);
    let me = || user.clone().map(|u| vec![("x-vr-user".to_string(), u)]).unwrap_or_default();
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
pub fn native_modules() -> Vec<&'static str> { vec!["auth"] }
