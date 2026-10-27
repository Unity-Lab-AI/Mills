//! The exchange between two copies, over HTTP on this machine (127.0.0.1), both ways: each copy PUSHES its own new
//! records to the other.
//!   GET  <peer>/__sync/hello?hub=<my id>   → { hub, name, have, last, kinds }  (`have`: how far it has my journal)
//!   POST <peer>/__sync/push  { hub, name, after, records: [...] } → { ok, have, applied, same, kept, deferred, refused, held }
//! Every call carries the shared secret in `x-vr-sync-secret` (compared in constant time). A call from another
//! machine, or one that came through a proxy (it carries a forwarding header: nginx, Cloudflare), is refused whatever
//! it carries. A peer that is down: the records wait in the journal, the sender asks again every few seconds and
//! catches up from the peer's own cursor.
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::engine::{Engine, Scan};

pub const SECRET_HEADER: &str = "x-vr-sync-secret";
/// Headers a proxy adds: a call carrying one did not come straight from the peer.
const PROXIED: [&str; 5] = ["x-forwarded-for", "x-real-ip", "cf-connecting-ip", "forwarded", "x-forwarded-host"];
const MAX_BATCH: usize = 5000;

/// Whether a call may reach the sync routes: from this machine, not through a proxy, with the secret.
pub fn gate(peer: SocketAddr, header: &dyn Fn(&str) -> Option<String>, secret: &str) -> Result<(), (u16, &'static str)> {
    use subtle::ConstantTimeEq;
    if !peer.ip().is_loopback() { return Err((403, "The sync answers only this machine.")); }
    if PROXIED.iter().any(|h| header(h).is_some()) { return Err((403, "The sync is not reachable through a proxy.")); }
    let given = header(SECRET_HEADER).unwrap_or_default();
    if secret.is_empty() || given.len() != secret.len() || !bool::from(given.as_bytes().ct_eq(secret.as_bytes())) { return Err((403, "Wrong or missing sync secret.")); }
    Ok(())
}

/// The answer to a call on `/__sync/<rest>` (already through [`gate`]). `push` applies records: call it off the async
/// threads (it reads and writes files).
pub fn answer(engine: &Engine, method: &str, rest: &str, query_hub: &str, body: &[u8]) -> (u16, Value) {
    match (method, rest) {
        ("GET", "hello") => (200, json!({ "hub": engine.id(), "name": engine.name(), "have": engine.have(query_hub), "last": engine.last_seq() })),
        ("POST", "push") => {
            let Ok(b) = serde_json::from_slice::<Value>(body) else { return (400, json!({ "error": "That is not JSON." })) };
            let Some(hub) = b["hub"].as_str().filter(|h| !h.is_empty() && h.len() <= 64) else { return (400, json!({ "error": "Say which hub sends (hub)." })) };
            if hub == engine.id() { return (400, json!({ "error": "That is this hub: a hub does not pair with itself." })); }
            let Some(recs) = b["records"].as_array() else { return (400, json!({ "error": "records must be a list." })) };
            if recs.len() > MAX_BATCH { return (413, json!({ "error": format!("At most {MAX_BATCH} records per call.") })); }
            let after = b["after"].as_u64().unwrap_or_else(|| engine.have(hub));
            let rep = engine.receive(hub, after, recs);
            (200, rep.to_value())
        }
        _ => (404, json!({ "error": "Not found" })),
    }
}

/// Looks for this copy's own changes, forever: changed kinds twice a second, kinds with no version every 3 s, everything
/// in full every minute.
pub async fn scan_loop(engine: Arc<Engine>) {
    let mut n: u64 = 0;
    loop {
        let level = if n % 120 == 0 { Scan::Full } else if n % 6 == 0 { Scan::Periodic } else { Scan::Changed };
        let e = engine.clone();
        let _ = tokio::task::spawn_blocking(move || e.scan(level)).await;
        n += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Sends this copy's records to the peer at `peer` (http://127.0.0.1:<port>), forever: hello (how far it is), the
/// records after that in batches, then waits for the journal to grow (or 5 s) and asks again.
pub async fn send_loop(engine: Arc<Engine>, peer: String, secret: String) {
    let peer = peer.trim_end_matches('/').to_string();
    let http = match pgm_net_internal::client::no_redirects(Duration::from_secs(3)) { Ok(h) => h, Err(e) => { eprintln!("[sync] no HTTP client: {e}"); return; } };
    loop {
        let hello = async {
            let r = http.get(format!("{peer}/__sync/hello?hub={}", engine.id())).header(SECRET_HEADER, &secret).timeout(Duration::from_secs(10)).send().await.map_err(|e| e.to_string())?;
            let st = r.status().as_u16();
            let v: Value = r.json().await.map_err(|e| e.to_string())?;
            if st != 200 { return Err(format!("it answered {st}: {}", v["error"].as_str().unwrap_or(""))); }
            Ok::<Value, String>(v)
        }.await;
        let h = match hello { Ok(h) => h, Err(e) => { engine.set_peer(false, "", "", 0, &e); tokio::time::sleep(Duration::from_secs(2)).await; continue; } };
        let peer_hub = h["hub"].as_str().unwrap_or("").to_string();
        if peer_hub.is_empty() || peer_hub == engine.id() { engine.set_peer(false, "", "", 0, "the peer is this hub itself"); tokio::time::sleep(Duration::from_secs(30)).await; continue; }
        let mut after = h["have"].as_u64().unwrap_or(0);
        engine.set_peer(true, &peer_hub, h["name"].as_str().unwrap_or(""), after, "");
        // the peer has more of this journal than there is (a restore went back): from what there is
        let last = engine.last_seq();
        if after > last { after = 0; }
        loop {
            let grew = engine.grew.notified();
            tokio::pin!(grew);
            grew.as_mut().enable();
            let e = engine.clone();
            let recs = tokio::task::spawn_blocking(move || e.outgoing(after, 4 << 20, 2000)).await.unwrap_or_default();
            if recs.is_empty() {
                tokio::select! { _ = &mut grew => {}, _ = tokio::time::sleep(Duration::from_secs(5)) => {} }
                break; // hello again: the peer's view is kept fresh, a restarted peer is noticed
            }
            let sent_last = recs.last().and_then(|r| r["seq"].as_u64()).unwrap_or(after);
            let body = json!({ "hub": engine.id(), "name": engine.name(), "after": after, "records": recs });
            let res = async {
                let r = http.post(format!("{peer}/__sync/push")).header(SECRET_HEADER, &secret).json(&body).timeout(Duration::from_secs(120)).send().await.map_err(|e| e.to_string())?;
                let st = r.status().as_u16();
                let v: Value = r.json().await.map_err(|e| e.to_string())?;
                if st != 200 { return Err(format!("it answered {st}: {}", v["error"].as_str().unwrap_or(""))); }
                Ok::<Value, String>(v)
            }.await;
            match res {
                Ok(a) => {
                    let e = engine.clone(); let a2 = a.clone();
                    let _ = tokio::task::spawn_blocking(move || e.acked(&recs, &a2)).await;
                    let have = a["have"].as_u64().unwrap_or(sent_last);
                    engine.set_peer(true, &peer_hub, h["name"].as_str().unwrap_or(""), have, "");
                    if have < sent_last { break; } // it did not take them all: ask where it is
                    after = have;
                }
                Err(err) => { engine.set_peer(false, "", "", 0, &err); tokio::time::sleep(Duration::from_secs(2)).await; break; }
            }
        }
    }
}
