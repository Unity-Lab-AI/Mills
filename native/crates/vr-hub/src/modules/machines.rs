//! Linked machines (plans/linked-machines.md 2.3, 3, T-309): the owner's list, rename, pause and unlink; the admin's
//! count; and what a machine itself calls with its device token (poll, signed reply, report, unlink itself).
//! The records are pgm-identity-internal's (`Devices`: a machine is a device token with `kind: "machine"`). Rust hub
//! only: the JavaScript hub knows none of these routes.
//! - Owner routes need the person's own page session (never a device or machine token) and only ever see their own.
//! - The admin's `GET /api/machines/count` is counts and online numbers per person: no names, no scopes (Sponge, 5).
//! - An unlink ends the machine's held poll within a quarter of a second and runs the unlink hook (`init`): the
//!   place T-311's job engine ends the machine's in-flight jobs ("machine unlinked").
use hyper::Method;
use pgm_ai_internal::machine_transfer::Dir;
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
pub fn init(auth: &Auth, ai: &Arc<pgm_ai_internal::ai_hosts::AiHosts>) {
    let jobs = ai.machines.clone();
    auth.devices.set_unlink_hook(Box::new(move |user, id| {
        // T-311: every tool call waiting on that machine fails now, and what was queued for it is dropped (the machine's own
        // runner ends its jobs when its next poll answers "revoked")
        jobs.drop_machine(id, "machine unlinked");
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
                a.devices.machine_report(tok, b["scope"].as_str(), b["mode"].as_str());
                let end = std::time::Instant::now() + std::time::Duration::from_secs(wait);
                let jobs = &hub.mods.ai.machines;
                let (hub_key, hub_kid) = jobs.public_key().unwrap_or_default();
                loop {
                    // listen first, then look: a job queued between the look and the wait is not lost
                    let wake = jobs.notify.notified();
                    tokio::pin!(wake);
                    wake.as_mut().enable();
                    match a.devices.machine_seen(tok, true) {
                        MachineSeen::Revoked => return Some(revoked()),
                        MachineSeen::Unknown => return Some(answer(401, json!({ "error": "Not signed in." }))),
                        MachineSeen::Linked { user, id, paused } => {
                            jobs.polled(&id);
                            // T-311: what is queued for this machine goes out now (nothing while it is paused from the web)
                            let out: Vec<Value> = if paused { vec![] } else { jobs.take(&id).iter().map(|j| j.to_json()).collect() };
                            if !out.is_empty() || std::time::Instant::now() >= end { return Some(answer(200, json!({ "ok": true, "machine": id, "owner": user, "paused": paused, "hubKey": hub_key, "hubKid": hub_kid, "jobs": out }))); }
                        }
                    }
                    tokio::select! { _ = &mut wake => {}, _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {} }
                }
            }
            p if p.starts_with("transfer/") => transfer_for_machine(hub, tok, &p["transfer/".len()..], r).await,
            "reply" => {
                let (rid, body, sig) = (b["requestId"].as_str().unwrap_or(""), b["body"].as_str().unwrap_or(""), b["sig"].as_str().unwrap_or(""));
                if rid.is_empty() || rid.len() > 128 { return Some(answer(400, json!({ "error": "requestId is needed." }))); }
                match a.devices.verify_machine_reply(tok, rid, body.as_bytes(), sig) {
                    Ok((owner, id)) => { let taken = hub.mods.ai.machines.deliver(&id, rid, body); answer(200, json!({ "ok": true, "machine": id, "owner": owner, "matched": taken })) } // the body goes to the call waiting for it (T-311)
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
    // ----- the owner's end of a big-file transfer (T-311): what a machine sent, and what a machine will fetch -----
    if p == "/api/transfers" || p.starts_with("/api/transfers/") { return Some(transfers_for_owner(hub, r).await); }
    // ----- the person's page: peek at a code, list, rename / pause, unlink; the admin's count -----
    if p !="/api/device/peek" && p != "/api/machines" && !p.starts_with("/api/machines/") { return None; }
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

fn now_s() -> i64 { (pgm_core_internal::now_ms() / 1000) as i64 }
fn bin(status: u16, buf: Vec<u8>) -> Routed {
    Routed::Answer(hyper::Response::builder().status(status).header("content-type", "application/octet-stream").header("content-length", buf.len()).header("cache-control", "private, no-store").body(crate::front::full(buf)).expect("response"))
}
fn xfail((s, m): (u16, String)) -> Routed { answer(s, json!({ "error": m })) }
fn xfail3((s, m, have): (u16, String, u64)) -> Routed { answer(s, json!({ "error": m, "received": have })) }

/// A machine's request that names a ticket (T-311): `status` (where the hub is), `chunk` (append one piece), `done` (check the
/// sha256 and keep the file), `data` (a range of the owner's file for a put). The ticket is checked against THIS machine's token:
/// its machine, its account, the path it names, the 5 minutes; nothing else gets through.
async fn transfer_for_machine(hub: &Arc<Hub>, tok: &str, rest: &str, r: &Req) -> Routed {
    let a = &hub.mods.auth;
    let x = &hub.mods.ai.machines.xfer;
    let (ticket, op) = rest.split_once('/').unwrap_or((rest, ""));
    let (user, id) = match a.devices.machine_seen(tok, false) {
        MachineSeen::Linked { user, id, paused } => { if paused { return answer(423, json!({ "error": "This machine is paused from the web." })); } (user, id) }
        MachineSeen::Revoked => return revoked(),
        MachineSeen::Unknown => return answer(401, json!({ "error": "Not signed in." })),
    };
    let b = r.json();
    let path = if op == "chunk" { r.param("path").unwrap_or_default() } else { b["path"].as_str().unwrap_or("").to_string() };
    let t = match x.check(ticket, &id, &user, &path, now_s()) { Ok(t) => t, Err(e) => { println!("[vr-hub] transfer: {user}'s machine {id} was refused ({} {})", e.0, e.1); return xfail(e); } };
    match op {
        "status" => {
            if t.dir == Dir::Get {
                let key = b["key"].as_str().unwrap_or("");
                let file = match x.bind(&t.id, key) { Ok(f) => f, Err(e) => return xfail(e) };
                match x.received(&t.account, &file) { Ok(n) => answer(200, json!({ "ok": true, "direction": "get", "received": n })), Err(e) => xfail(e) }
            } else { answer(200, json!({ "ok": true, "direction": "put", "received": 0, "file": t.file })) }
        }
        "chunk" if t.dir == Dir::Get => {
            let key = r.param("key").unwrap_or_default();
            let file = match x.bind(&t.id, &key) { Ok(f) => f, Err(e) => return xfail(e) };
            let offset: u64 = r.param("offset").and_then(|o| o.parse().ok()).unwrap_or(u64::MAX);
            let (restart, body, h2, t2) = (r.param("restart").as_deref() == Some("1"), r.body.clone(), hub.clone(), t.clone());
            match tokio::task::spawn_blocking(move || h2.mods.ai.machines.xfer.append(&t2, &file, offset, &body, restart)).await {
                Ok(Ok(n)) => answer(200, json!({ "ok": true, "received": n })),
                Ok(Err(e)) => xfail3(e),
                Err(_) => answer(500, json!({ "error": "The transfer failed (a panic inside it); it was logged." })),
            }
        }
        "done" if t.dir == Dir::Get => {
            let key = b["key"].as_str().unwrap_or("").to_string();
            let file = match x.bind(&t.id, &key) { Ok(f) => f, Err(e) => return xfail(e) };
            let (size, sha) = (b["size"].as_u64().unwrap_or(u64::MAX), b["sha256"].as_str().unwrap_or("").to_string());
            let name = path.rsplit(['/', '\\']).next().unwrap_or("file").to_string();
            let (h2, t2) = (hub.clone(), t.clone());
            match tokio::task::spawn_blocking(move || h2.mods.ai.machines.xfer.finish_get(&t2, &file, size, &sha, &name)).await {
                Ok(Ok(v)) => { println!("[vr-hub] transfer: {user}'s machine {id} sent {path} ({size} bytes) to the hub's transfer area"); answer(200, v) }
                Ok(Err(e)) => xfail(e),
                Err(_) => answer(500, json!({ "error": "The transfer failed (a panic inside it); it was logged." })),
            }
        }
        "data" if t.dir != Dir::Get => {
            let (offset, len) = (b["offset"].as_u64().unwrap_or(0), b["len"].as_u64().unwrap_or(0) as usize);
            let (h2, t2) = (hub.clone(), t.clone());
            match tokio::task::spawn_blocking(move || h2.mods.ai.machines.xfer.read_range(&t2, offset, len)).await {
                Ok(Ok(buf)) => bin(200, buf),
                Ok(Err(e)) => xfail(e),
                Err(_) => answer(500, json!({ "error": "The transfer failed (a panic inside it); it was logged." })),
            }
        }
        _ => answer(404, json!({ "error": "That ticket does not do that." })),
    }
}

/// `/api/transfers`: the owner's end. Any session or token of the account (never another account's): list, make an upload
/// slot, add chunks, finish, download with Range (streamed from disk), delete.
async fn transfers_for_owner(hub: &Arc<Hub>, r: &Req) -> Routed {
    let a = &hub.mods.auth;
    let Some(u) = a.session_user(r) else { return answer(401, json!({ "error": "Not signed in." })) };
    let x = &hub.mods.ai.machines.xfer;
    x.sweep();
    let rest = r.path.strip_prefix("/api/transfers").unwrap_or("").trim_start_matches('/');
    let (id, op) = rest.split_once('/').unwrap_or((rest, ""));
    let m = &r.method;
    if id.is_empty() {
        return if m == Method::GET { answer(200, json!({ "transfers": x.list(&u) })) }
        else if m == Method::POST { match x.upload_new(&u, r.json()["name"].as_str().unwrap_or("")) { Ok(v) => answer(200, v), Err(e) => xfail(e) } }
        else { answer(405, json!({ "error": "Method not allowed." })) };
    }
    match (m.clone(), op) {
        (Method::POST, "chunk") => {
            let offset: u64 = r.param("offset").and_then(|o| o.parse().ok()).unwrap_or(u64::MAX);
            let (h2, u2, id2, body) = (hub.clone(), u.clone(), id.to_string(), r.body.clone());
            match tokio::task::spawn_blocking(move || h2.mods.ai.machines.xfer.upload_append(&u2, &id2, offset, &body)).await {
                Ok(Ok(n)) => answer(200, json!({ "ok": true, "received": n })),
                Ok(Err(e)) => xfail3(e),
                Err(_) => answer(500, json!({ "error": "The upload failed (a panic inside it); it was logged." })),
            }
        }
        (Method::POST, "done") => {
            let (h2, u2, id2) = (hub.clone(), u.clone(), id.to_string());
            match tokio::task::spawn_blocking(move || h2.mods.ai.machines.xfer.upload_done(&u2, &id2)).await { Ok(Ok(v)) => answer(200, v), Ok(Err(e)) => xfail(e), Err(_) => answer(500, json!({ "error": "The upload failed; it was logged." })) }
        }
        (Method::GET, "") => match x.meta(&u, id) {
            None => answer(404, json!({ "error": "No such transfer." })),
            Some(meta) => {
                if r.param("info").is_some() || meta["complete"] != json!(true) { return answer(200, meta); }
                let Some((file, size, name)) = x.open(&u, id) else { return answer(404, json!({ "error": "No such transfer." })) };
                download(file, size, &name, meta["sha256"].as_str().unwrap_or(""), r.header("range"))
            }
        },
        (Method::DELETE, "") => if x.delete(&u, id) { answer(200, json!({ "ok": true })) } else { answer(404, json!({ "error": "No such transfer." })) },
        _ => answer(405, json!({ "error": "Method not allowed." })),
    }
}

/// A file from disk to the person, streamed in 1 MB pieces (the hub holds a few at a time, whatever the size), with `Range`.
fn download(mut file: std::fs::File, size: u64, name: &str, sha: &str, range: Option<&str>) -> Routed {
    use std::io::{Read, Seek, SeekFrom};
    let (start, end, partial) = match range.and_then(|h| h.strip_prefix("bytes=")).and_then(|s| s.split_once('-')) {
        Some((a, b)) => {
            let (s, e) = (a.trim().parse::<u64>().ok(), b.trim().parse::<u64>().ok());
            match (s, e) { (Some(s), Some(e)) => (s, e.min(size.saturating_sub(1)), true), (Some(s), None) => (s, size.saturating_sub(1), true), (None, Some(n)) => (size.saturating_sub(n), size.saturating_sub(1), true), _ => (0, size.saturating_sub(1), false) }
        }
        None => (0, size.saturating_sub(1), false),
    };
    if size > 0 && (start >= size || start > end) { return Routed::Answer(hyper::Response::builder().status(416).header("content-range", format!("bytes */{size}")).body(crate::front::full(Vec::new())).expect("response")); }
    let len = if size == 0 { 0 } else { end - start + 1 };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, String>>(4);
    tokio::task::spawn_blocking(move || {
        if file.seek(SeekFrom::Start(start)).is_err() { return; }
        let (mut left, mut buf) = (len, vec![0u8; 1 << 20]);
        while left > 0 {
            let n = (left as usize).min(buf.len());
            match file.read(&mut buf[..n]) { Ok(0) | Err(_) => break, Ok(k) => { left -= k as u64; if tx.blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..k]))).is_err() { return; } } }
        }
    });
    let mut b = hyper::Response::builder().status(if partial { 206 } else { 200 }).header("content-type", "application/octet-stream").header("content-length", len).header("accept-ranges", "bytes")
        .header("content-disposition", format!("attachment; filename=\"{}\"", vr_core::hub::urlencode(name))).header("x-vr-sha256", sha).header("cache-control", "private, no-store").header("x-content-type-options", "nosniff");
    if partial { b = b.header("content-range", format!("bytes {start}-{end}/{size}")); }
    Routed::Answer(b.body(pgm_ai_internal::hub::stream_body(rx)).expect("response"))
}
