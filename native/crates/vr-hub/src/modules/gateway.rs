//! PGM Gateway (5.1.0): this hub is also the gateway its products (PGM Prod Suit, ShaDow Chat, Hyper Prod Man; each its
//! own program, data and accounts) ask for what only it has: the identity directory (VibeRoller logins, for a product's
//! first sign-in) and the AI hosts (their sharing exactly as for VibeRoller, nothing changed on the hosts).
//!
//! - Products are registered by an admin (`/api/admin/gateway/products`). Each gets a secret, kept in
//!   `bridge/gateway/products.json` (owner-only) and in `bridge/gateway/<id>.secret` for the product's program on this
//!   machine to read: it is never shown in an answer.
//! - A product's requests are signed (pgm-net-internal's product link) and come from this machine only:
//!   `/internal/gateway/*` (nginx never passes /internal/ from outside).
//! - A product acts only for people it linked: someone who signed in to it with their VibeRoller login
//!   (`identity/verify`). Links are per product (`bridge/gateway/links/<id>.json`); an admin's "remove" ends them.
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::auth::Account;
use super::Routed;
use crate::front::{json as jresp, Hub, Req};
use pgm_ai_internal::hub::AiHub;
use pgm_core_internal::json::{read_json, write_json_private};
use pgm_net_internal::product::{valid_id, Signed, Verifier};

pub const NAME: &str = "PGM Gateway";

pub struct Gateway {
    dir: PathBuf,
    products: Mutex<Map<String, Value>>, // id → { name, secret, ai, created }
    links: Mutex<Map<String, Value>>,    // id → { username → { at } }
    verifier: Verifier,
}
impl Gateway {
    pub fn new(root: &std::path::Path) -> Gateway {
        let dir = root.join("gateway");
        let products = read_json(&dir.join("products.json")).and_then(|v| v["products"].as_object().cloned()).unwrap_or_default();
        let mut links = Map::new();
        for id in products.keys() { links.insert(id.clone(), read_json(&dir.join("links").join(format!("{id}.json"))).filter(Value::is_object).unwrap_or(json!({}))); }
        Gateway { dir, products: Mutex::new(products), links: Mutex::new(links), verifier: Verifier::default() }
    }
    fn save(&self) {
        let _ = pgm_core_internal::fs::private_dir(&self.dir);
        write_json_private(&self.dir.join("products.json"), &json!({ "products": Value::Object(self.products.lock().unwrap().clone()) }));
    }
    fn save_links(&self, id: &str) {
        let d = self.dir.join("links"); let _ = pgm_core_internal::fs::private_dir(&self.dir); let _ = pgm_core_internal::fs::private_dir(&d);
        let v = self.links.lock().unwrap().get(id).cloned().unwrap_or(json!({}));
        write_json_private(&d.join(format!("{id}.json")), &v);
    }
    fn secret_of(&self, id: &str) -> Option<String> { self.products.lock().unwrap().get(id).and_then(|p| p["secret"].as_str().map(String::from)) }
    fn linked(&self, id: &str, user: &str) -> bool { !user.is_empty() && self.links.lock().unwrap().get(id).map(|l| l.get(user).is_some()).unwrap_or(false) }
    fn link(&self, id: &str, user: &str) {
        { let mut l = self.links.lock().unwrap(); let e = l.entry(id.to_string()).or_insert(json!({})); e[user] = json!({ "at": pgm_core_internal::time::iso_now() }); }
        self.save_links(id);
    }
    /// What an admin sees: no secrets.
    pub fn list(&self) -> Vec<Value> {
        let links = self.links.lock().unwrap();
        self.products.lock().unwrap().iter().map(|(id, p)| json!({ "id": id, "name": p["name"], "ai": p["ai"] == json!(true), "created": p["created"], "linked": links.get(id).and_then(Value::as_object).map(|m| m.len()).unwrap_or(0), "secretFile": format!("bridge/gateway/{id}.secret") })).collect()
    }
    /// A new product, or a new secret for one (the old one stops working at once). Its links stay.
    pub fn put(&self, id: &str, name: &str, ai: bool) -> Result<Value, String> {
        if !valid_id(id) { return Err("A product id is 1–32 of a-z, 0-9 and \"-\".".into()); }
        let name = name.trim(); if name.is_empty() || name.chars().count() > 64 { return Err("A product needs a name (up to 64 characters).".into()); }
        let secret = pgm_core_internal::crypto::random_hex(32);
        { let mut ps = self.products.lock().unwrap(); let created = ps.get(id).map(|p| p["created"].clone()).unwrap_or_else(|| json!(pgm_core_internal::time::iso_now())); ps.insert(id.into(), json!({ "name": name, "secret": secret, "ai": ai, "created": created })); }
        self.links.lock().unwrap().entry(id.to_string()).or_insert(json!({}));
        self.save();
        let f = self.dir.join(format!("{id}.secret"));
        std::fs::write(&f, format!("{secret}\n")).map_err(|e| format!("could not write {}: {e}", f.display()))?;
        pgm_core_internal::fs::private(&f);
        Ok(self.list().into_iter().find(|p| p["id"] == json!(id)).unwrap_or(Value::Null))
    }
    /// A product goes: its secret and its links with it.
    pub fn remove(&self, id: &str) -> bool {
        let had = self.products.lock().unwrap().remove(id).is_some();
        self.links.lock().unwrap().remove(id);
        if had { self.save(); let _ = std::fs::remove_file(self.dir.join(format!("{id}.secret"))); let _ = std::fs::remove_file(self.dir.join("links").join(format!("{id}.json"))); }
        had
    }
    fn ai_on(&self, id: &str) -> bool { self.products.lock().unwrap().get(id).map(|p| p["ai"] == json!(true)).unwrap_or(false) }
}

fn answer(status: u16, v: Value) -> Routed { Routed::Answer(jresp(status, &v)) }

/// `/internal/gateway/<rest>`: a product's signed request.
pub async fn route(hub: &Arc<Hub>, r: &Req, rest: &str) -> Routed {
    let g = &hub.mods.gateway;
    let Signed { product, user } = match g.verifier.check(r, &|id| g.secret_of(id)) {
        Ok(s) => s,
        Err(e) => { println!("[vr-hub] gateway: refused {} {} ({e})", r.method, r.path); return answer(403, json!({ "error": "forbidden" })); }
    };
    let get = r.method == hyper::Method::GET; let post = r.method == hyper::Method::POST;
    let account = |u: &str| -> Option<Account> { if g.linked(&product, u) { hub.mods.auth.account(u) } else { None } };
    match rest {
        "status" if get => {
            let hosts = hub.mods.ai.status();
            let up = hosts.iter().filter(|h| h["online"] == json!(true)).count();
            answer(200, json!({ "gateway": NAME, "version": crate::VERSION, "product": product, "ai": g.ai_on(&product), "hostsUp": up }))
        }
        // someone signs in to the product with their VibeRoller login: is it theirs? (the same limits as a sign-in)
        "identity/verify" if post => {
            let b = r.json();
            let (u, pw, ip) = (b["username"].as_str().unwrap_or(""), b["password"].as_str().unwrap_or(""), b["ip"].as_str().unwrap_or(""));
            if pw.is_empty() || pw.len() > 1024 { return answer(400, json!({ "error": "username and password are needed." })); }
            match hub.mods.auth.verify_password(u, pw, ip).await {
                Ok(a) => { g.link(&product, &a.username); println!("[vr-hub] gateway: {} linked {} (their VibeRoller login)", product, a.username); answer(200, json!({ "ok": true, "username": a.username, "display": hub.mods.name_of(&a.username) })) }
                Err((st, e)) => answer(st, json!({ "ok": false, "error": e })),
            }
        }
        "identity/profile" if get => match account(&user) {
            Some(a) => answer(200, json!({ "username": a.username, "display": hub.mods.name_of(&a.username) })),
            None => answer(404, json!({ "error": "Not linked to this product." })),
        },
        "ai/providers" if get => {
            if !g.ai_on(&product) { return answer(403, json!({ "error": "This product has no AI from the gateway." })); }
            let Some(a) = account(&user) else { return answer(404, json!({ "error": "Not linked to this product." })) };
            let list: Vec<Value> = super::work::providers_list(&**hub, &a.username).into_iter().map(|p| json!({ "id": p["id"], "kind": p["kind"], "label": p["label"], "models": p["models"] })).collect();
            answer(200, json!({ "providers": list }))
        }
        v if v == "v1" || v.starts_with("v1/") => {
            if !g.ai_on(&product) { return answer(403, json!({ "error": "This product has no AI from the gateway." })); }
            let Some(a) = account(&user) else { return answer(404, json!({ "error": "Not linked to this product." })) };
            let inner = Req { method: r.method.clone(), path: format!("/{v}"), query: r.query.clone(), headers: r.headers.clone(), body: r.body.clone(), peer: r.peer };
            let d: Arc<dyn AiHub> = hub.clone();
            Routed::Answer(pgm_ai_internal::openai::v1_as(&d, &inner, a.username).await)
        }
        _ => answer(404, json!({ "error": "Not found" })),
    }
}

/// The admin's products: `GET /api/admin/gateway/products`, `POST …/products {id, name, ai}` (new, or a new secret),
/// `POST …/products/remove {id}`.
pub fn admin(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    let p = r.path.as_str();
    if p != "/api/admin/gateway/products" && p != "/api/admin/gateway/products/remove" { return None; }
    if !a.admin { return Some(answer(403, json!({ "error": "Admins only." }))); }
    let g = &hub.mods.gateway;
    let b = r.json();
    Some(match (r.method.as_str(), p) {
        ("GET", "/api/admin/gateway/products") => answer(200, json!({ "gateway": NAME, "products": g.list() })),
        ("POST", "/api/admin/gateway/products") => match g.put(b["id"].as_str().unwrap_or(""), b["name"].as_str().unwrap_or(""), b["ai"].as_bool().unwrap_or(false)) {
            Ok(v) => { println!("[vr-hub] gateway: {} set product {}", a.username, v["id"]); answer(200, json!({ "product": v })) }
            Err(e) => answer(400, json!({ "error": e })),
        },
        ("POST", "/api/admin/gateway/products/remove") => { let id = b["id"].as_str().unwrap_or(""); if g.remove(id) { println!("[vr-hub] gateway: {} removed product {id}", a.username); answer(200, json!({ "ok": true })) } else { answer(404, json!({ "error": "No such product." })) } }
        _ => answer(405, json!({ "error": "Method not allowed." })),
    })
}
