//! Linked machines (plans/linked-machines.md 2.3, 3, T-309): the owner's list, rename, pause and unlink; the admin's
//! count; and what a machine itself calls with its device token (poll, signed reply, report, unlink itself).
//! The records are pgm-identity-internal's (`Devices`: a machine is a device token with `kind: "machine"`). Rust hub
//! only: the JavaScript hub knows none of these routes.
//! - Owner routes need the person's own page session (never a device or machine token) and only ever see their own.
//! - The admin's `GET /api/machines/count` is counts and online numbers per person: no names, no scopes (Sponge, 5).
//! - An unlink ends the machine's held poll within a quarter of a second and runs the unlink hook (`init`): the
//!   place T-311's job engine ends the machine's in-flight jobs ("machine unlinked").
use hyper::Method;
use pgm_identity_internal::devices::MachineSeen;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::front::{json as jresp, Hub, Req};
use crate::modules::auth::Auth;
use crate::modules::Routed;

fn answer(status: u16, v: Value) -> Routed { Routed::Answer(jresp(status, &v)) }
const MAX_WAIT_S: u64 = 25;

/// What happens when a machine is unlinked or revoked (the web's Unlink, a password change, "sign out everywhere", a
/// re-link that replaces it). Its poll is closed by the poll loop (it asks `machine_seen` every tick and gets
/// "revoked"); in-flight jobs end here once the job engine exists (T-311).
pub fn init(auth: &Auth) {
    auth.devices.set_unlink_hook(Box::new(|user, id| {
        println!("[vr-hub] machine {id} of {user} is unlinked: its poll is closed and its jobs end (machine unlinked).");
    }));
}

fn bearer(r: &Req) -> Option<&str> { r.header("authorization").and_then(|v| v.strip_prefix("Bearer ")) }

fn revoked() -> Routed { answer(401, json!({ "error": "revoked", "revoked": true, "message": "This machine was unlinked on the web. Wipe the token and the key, and link again if you want it back." })) }

pub async fn route(hub: &Arc<Hub>, r: &Req) -> Option<Routed> {
    let p = r.path.as_str();
    let a = &hub.mods.auth;
    // ----- the machine's own calls, with its device token -----
    if let Some(rest) = p.strip_prefix("/api/machine/") {
        if r.method != Method::POST { return Some(answer(405, json!({ "error": "Method not allowed." }))); }
        let Some(tok) = bearer(r) else { return Some(answer(401, json!({ "error": "Not signed in." }))) };
        let b = r.json();
        return Some(match rest {
            "poll" => {
                // the held poll: online while it is held; closed at once when the machine is unlinked
                let wait = b["wait"].as_u64().unwrap_or(0).min(MAX_WAIT_S);
                // a scope DOCUMENT in the poll (`scopeDoc`) is read here and refused whole when it is newer than this hub reads (T-310,
                // section 9): the words come from the parsed document, nothing of a refused one is kept; the poll itself still answers
                let (mut words, mut refused) = (b["scope"].as_str().map(String::from), None::<String>);
                if !b["scopeDoc"].is_null() {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
                    match pgm_net_internal::compat::scope_from_poll(&b["scopeDoc"], now) { Ok(w) => words = Some(w), Err(e) => { words = None; refused = Some(e); } }
                }
                a.devices.machine_report(tok, words.as_deref(), b["mode"].as_str());
                let end = std::time::Instant::now() + std::time::Duration::from_secs(wait);
                loop {
                    match a.devices.machine_seen(tok, true) {
                        MachineSeen::Revoked => return Some(revoked()),
                        MachineSeen::Unknown => return Some(answer(401, json!({ "error": "Not signed in." }))),
                        MachineSeen::Linked { user, id, paused } => {
                            if std::time::Instant::now() >= end { return Some(answer(200, json!({ "ok": true, "machine": id, "owner": user, "paused": paused, "jobs": [], "scopeRefused": refused }))); }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
            }
            "reply" => {
                let (rid, body, sig) = (b["requestId"].as_str().unwrap_or(""), b["body"].as_str().unwrap_or(""), b["sig"].as_str().unwrap_or(""));
                if rid.is_empty() || rid.len() > 128 { return Some(answer(400, json!({ "error": "requestId is needed." }))); }
                match a.devices.verify_machine_reply(tok, rid, body.as_bytes(), sig) {
                    Ok((owner, id)) => answer(200, json!({ "ok": true, "machine": id, "owner": owner })), // (T-311 hands the body to the waiting job here)
                    Err((401, e)) if e == "revoked" => revoked(),
                    Err((s, e)) => answer(s, json!({ "error": e })),
                }
            }
            "unlink" => match a.devices.machine_seen(tok, false) {
                MachineSeen::Linked { user, id, .. } => { a.devices.revoke(&user, &id); println!("[vr-hub] {user}'s machine {id} unlinked itself."); answer(200, json!({ "ok": true })) }
                MachineSeen::Revoked => revoked(),
                MachineSeen::Unknown => answer(401, json!({ "error": "Not signed in." })),
            },
            _ => answer(404, json!({ "error": "No such route." })),
        });
    }
    // ----- the person's page: peek at a code, list, rename / pause, unlink; the admin's count -----
    if p != "/api/device/peek" && p != "/api/machines" && !p.starts_with("/api/machines/") { return None; }
    let Some(u) = a.page_session_user(r) else { return Some(answer(if bearer(r).is_some() && a.session_user(r).is_some() { 403 } else { 401 }, json!({ "error": "Open the page and sign in with your own session." }))) };
    if p == "/api/device/peek" && r.method == Method::POST {
        return Some(match a.devices.peek(&u, r.json()["userCode"].as_str().unwrap_or("")) { Ok(v) => answer(200, v), Err((s, e)) => answer(s, json!({ "error": e })) });
    }
    if p == "/api/machines" && r.method == Method::GET { return Some(answer(200, json!({ "machines": a.devices.machines_of(&u) }))); }
    if p == "/api/machines/count" && r.method == Method::GET {
        if !a.is_admin(&u) { return Some(answer(403, json!({ "error": "Admins only." }))); }
        let per = a.devices.machine_counts();
        let (n, on) = per.iter().fold((0, 0), |(n, on), x| (n + x["machines"].as_u64().unwrap_or(0), on + x["online"].as_u64().unwrap_or(0)));
        return Some(answer(200, json!({ "machines": n, "online": on, "people": per })));
    }
    if let Some(id) = p.strip_prefix("/api/machines/").filter(|i| !i.is_empty() && !i.contains('/')) {
        if r.method == Method::PATCH {
            let b = r.json();
            if b["name"].is_null() && b["paused"].is_null() { return Some(answer(400, json!({ "error": "name and / or paused." }))); }
            if !b["name"].is_null() && b["name"].as_str().map(|n| n.trim().is_empty()).unwrap_or(true) { return Some(answer(400, json!({ "error": "name must be some text." }))); }
            return Some(if a.devices.machine_update(&u, id, b["name"].as_str(), b["paused"].as_bool()) { answer(200, json!({ "ok": true, "machine": a.devices.machines_of(&u).into_iter().find(|m| m["id"].as_str() == Some(id)) })) } else { answer(404, json!({ "error": "No such machine." })) });
        }
        if r.method == Method::DELETE {
            return Some(if a.devices.machines_of(&u).iter().any(|m| m["id"].as_str() == Some(id)) && a.devices.revoke(&u, id) { println!("[vr-hub] {u} unlinked machine {id}."); answer(200, json!({ "ok": true })) } else { answer(404, json!({ "error": "No such machine." })) });
        }
    }
    Some(answer(405, json!({ "error": "Method not allowed." })))
}
