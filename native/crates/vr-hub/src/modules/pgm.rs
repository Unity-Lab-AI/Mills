//! The Jcode PGM Server next to the hub (T-376, stage 1; team note plans/jcode-pgm-server.md, B and B.1): the hub vouches
//! for who a person is, the server serves the team's OpenRouter pairs to jcode clients, and keys, pairs and budgets stay
//! the hub's (one source of truth).
//! - `/pgm/*`: forwarded to the server (config `pgmServer.url`, http://127.0.0.1:<port>), path and query unchanged,
//!   streamed both ways and never read. Every path needs a bearer device token or page session (401), except `GET
//!   /pgm/info`; a linked machine's token is refused (403) everywhere. The client's `authorization`, `cookie`, `x-pgm-*`
//!   and `x-vr-*` headers never go on; the hub adds `x-pgm-hub-user` (who it is, signed with the shared secret). No
//!   `pgmServer.url` = 404 "this hub has no Jcode PGM Server".
//! - `/__pgm/*`: the server asking the hub (this machine only, never through a proxy, `x-pgm-secret` = the first line of
//!   `bridge/pgm-server.secret`, made owner-only at the start; the gate is the one /__sync/ uses): `whoami`, `models`,
//!   `route` (the key for one request), `charge` (the same records as the hub's own OpenRouter calls), `sessions/report`.
//!   Every call is in the security log (`pgm.*`), never a key or a token.
//! - `/api/check` gains `pgmServer: { url, signKey }` when the server is set up and its sign key is known.
//! Config is read fresh on every request (no restart). The JavaScript hub has none of this (404).
use hyper::body::Incoming;
use hyper::{Request, Response};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{answer, Config, Routed};
use crate::front::{json as jresp, Body, Hub, Req};
use pgm_ai_internal::hub::AiHub;
use pgm_core_internal::audit;
use pgm_identity_internal::TokenWho;
use pgm_net_internal::http::{local_gate, stream_body, Refused, Stream};

pub const SECRET_FILE: &str = "pgm-server.secret";
pub const SECRET_HEADER: &str = "x-pgm-secret";
pub const USER_HEADER: &str = "x-pgm-hub-user";
pub const NO_SERVER: &str = "this hub has no Jcode PGM Server";
/// The source the server's session reports carry.
pub const SOURCE: &str = "pgm-server";
/// A request's body passed on is cut off past this (as the hub's own requests are).
const MAX_BODY: usize = 32 << 20;
/// How long the server's answer head may take (a non-streamed completion can take minutes); the body has no limit.
const HEAD_WAIT: Duration = Duration::from_secs(600);

pub struct Pgm { secret: String, stream: Stream }
impl Pgm {
    /// The shared secret (made when missing) and the forward's client.
    pub fn new(root: &Path) -> Pgm {
        let f = root.join(SECRET_FILE);
        let secret = match pgm_core_internal::fs::secret_file(&f, 32) { Ok(s) => s, Err(e) => { eprintln!("[vr-hub] pgm: could not read or make {}: {e} (the Jcode PGM Server API refuses every call)", f.display()); String::new() } };
        Pgm { secret, stream: Stream::new(Duration::from_secs(3)) }
    }
}

/// config `pgmServer`, read fresh (config.json, config.local.json over it).
fn conf(root: &Path) -> Value { Config::load(root).get("pgmServer").cloned().filter(Value::is_object).unwrap_or(Value::Null) }
/// The server's address: Some(Ok(base)) when it is an address on this machine, Some(Err(why)) when it is set but not one,
/// None when there is none.
fn server_url(c: &Value) -> Option<Result<String, String>> {
    let u = c["url"].as_str().map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty())?;
    let local = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"].iter().any(|p| u.strip_prefix(p).is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())));
    Some(if local { Ok(u) } else { Err(format!("pgmServer.url must be http://127.0.0.1:<port> (the server on this machine), not \"{}\"", u.chars().take(80).collect::<String>())) })
}
fn bearer(h: &hyper::HeaderMap) -> Option<&str> { h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim).filter(|t| !t.is_empty()) }

/// `/pgm/*`: the forward (called before the request's body is read, so it streams).
pub async fn forward(hub: &Arc<Hub>, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();
    let no = |st: u16, e: &str| jresp(st, &json!({ "error": e }));
    // not for another site's page (the API's bearer is no cookie, but a page has no business here)
    if let Some(o) = parts.headers.get("origin").and_then(|v| v.to_str().ok()) { if !hub.allowed_origin(o) { return no(403, "Origin not allowed"); } }
    let c = conf(&hub.root);
    let base = match server_url(&c) { None => return no(404, NO_SERVER), Some(Err(why)) => { eprintln!("[vr-hub] pgm: {why}"); return no(503, &format!("The Jcode PGM Server is not set up right on this hub: {why}.")); } Some(Ok(b)) => b };
    let who = match bearer(&parts.headers) {
        None => TokenWho::Unknown,
        Some(t) => match hub.mods.auth.token_who(t) {
            TokenWho::Machine => {
                audit::record("", "pgm.refused", &path, json!({ "why": "a linked machine's token" }), &hub.mods.auth.outside_ip(&head_req(&parts, peer)));
                return no(403, "A linked machine's token reaches only /api/machine/ (poll, reply, unlink).");
            }
            w => w,
        },
    };
    let open = parts.method == hyper::Method::GET && path == "/pgm/info";
    let mut extra: Vec<(String, String)> = vec![];
    match &who {
        TokenWho::User { user, device_id, .. } => extra.push((USER_HEADER.into(), pgm_core_internal::crypto::vouch(hub.mods.pgm.secret.as_bytes(), &json!({ "user": user, "deviceId": device_id, "at": pgm_core_internal::now_ms() })))),
        _ if !open => return no(401, "Sign in to VibeRoller first: the Jcode PGM Server needs your device token (or session) as the bearer."),
        _ => {}
    }
    let pq = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| path.clone());
    let keep = |k: &str| !(k == "authorization" || k == "cookie" || k.starts_with("x-pgm-") || k.starts_with("x-vr-"));
    match hub.mods.pgm.stream.send(&format!("{base}{pq}"), parts.method.clone(), &parts.headers, keep, &extra, stream_body(body, MAX_BODY), HEAD_WAIT).await {
        Ok(mut r) => {
            // nginx in front must not hold a stream back
            if !r.headers().contains_key("x-accel-buffering") { r.headers_mut().insert("x-accel-buffering", hyper::header::HeaderValue::from_static("no")); }
            r
        }
        Err((st, why)) => no(st, &format!("The Jcode PGM Server could not be reached: {why}.")),
    }
}
/// The server for Work (stage 3, F.4): its address and the shared secret, when `pgmServer.url` is an address on this machine
/// and the secret is there (read fresh, like the rest of the config).
pub fn work_server(hub: &Hub) -> Option<(String, String)> {
    let base = server_url(&conf(&hub.root))?.ok()?;
    let secret = hub.mods.pgm.secret.clone();
    (!secret.is_empty()).then_some((base, secret))
}
/// The request's head as a [`Req`] (no body): for the helpers that look at headers only.
fn head_req(parts: &hyper::http::request::Parts, peer: SocketAddr) -> Req {
    Req { method: parts.method.clone(), path: parts.uri.path().to_string(), query: parts.uri.query().unwrap_or("").to_string(), headers: parts.headers.clone(), body: bytes::Bytes::new(), peer }
}

/// `/__pgm/<rest>`: the server asking the hub.
pub async fn api(hub: &Arc<Hub>, r: &Req, rest: &str) -> Routed {
    let ip = hub.mods.auth.outside_ip(r);
    if let Err(why) = local_gate(r.peer, &|k| r.header(k).map(String::from), SECRET_HEADER, &hub.mods.pgm.secret) {
        let why = match why { Refused::NotLocal => "The Jcode PGM Server API answers only this machine.", Refused::Proxied => "The Jcode PGM Server API is not reachable through a proxy.", Refused::Secret => "Wrong or missing x-pgm-secret." };
        eprintln!("[vr-hub] pgm: refused {} /__pgm/{rest} from {}: {why}", r.method, r.peer.ip());
        audit::record("", "pgm.refused", &format!("/__pgm/{}", rest.chars().take(40).collect::<String>()), json!({ "why": why }), &ip);
        return answer(403, json!({ "error": why }));
    }
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let b = r.json();
    let ah: Arc<dyn AiHub> = hub.clone();
    let acct = |u: &str| if super::auth::valid_username(u) { hub.mods.auth.account(u) } else { None };
    let no_user = |u: &str| answer(404, json!({ "error": format!("No such account: \"{}\".", u.chars().take(40).collect::<String>()) }));
    let sv = |k: &str| b[k].as_str().unwrap_or("").trim().to_string();
    match rest {
        "whoami" if post => match hub.mods.auth.token_who(b["token"].as_str().unwrap_or("").trim()) {
            TokenWho::User { user, device_id, kind } => {
                audit::record(&user, "pgm.whoami", &user, json!({ "kind": kind }), &ip);
                answer(200, json!({ "username": user, "deviceId": device_id, "admin": hub.mods.auth.is_admin(&user), "kind": kind }))
            }
            TokenWho::Machine => { audit::record("", "pgm.whoami", "", json!({ "refused": "machine" }), &ip); answer(403, json!({ "error": "A linked machine's token is not a sign-in." })) }
            TokenWho::Unknown => { audit::record("", "pgm.whoami", "", json!({ "refused": "unknown" }), &ip); answer(401, json!({ "error": "Unknown or expired token." })) }
        },
        "models" if get => {
            let u = r.param("user").unwrap_or_default().trim().to_lowercase();
            if acct(&u).is_none() { return no_user(&u); }
            let pairs = pgm_ai_internal::openrouter::pgm_pairs(&*ah, &u);
            audit::record(&u, "pgm.models", &u, json!({ "pairs": pairs.len() }), &ip);
            answer(200, json!({ "pairs": pairs, "budget": pgm_ai_internal::openrouter::pgm_budget(&*ah, &u) }))
        }
        "route" if post => {
            let (u, pair) = (sv("user").to_lowercase(), sv("pair"));
            if acct(&u).is_none() { return no_user(&u); }
            match pgm_ai_internal::openrouter::pgm_route(&*ah, &u, &pair) {
                Ok(v) => { audit::record(&u, "pgm.route", &pair, json!({ "ok": true, "kind": v["kind"], "model": v["model"] }), &ip); answer(200, v) }
                Err((st, e)) => { audit::record(&u, "pgm.route", &pair, json!({ "ok": false, "status": st }), &ip); answer(st, json!({ "error": e })) }
            }
        }
        "charge" if post => {
            let (u, pair, kind, model) = (sv("user").to_lowercase(), sv("pair"), sv("kind"), sv("model"));
            if acct(&u).is_none() { return no_user(&u); }
            let usd = b["usd"].as_f64().filter(|x| x.is_finite() && *x >= 0.0 && *x <= 10_000.0);
            let tok = |k: &str| if b[k].is_null() { Some(0) } else { b[k].as_u64() };
            let (Some(usd), Some(tin), Some(tout)) = (usd, tok("tokensIn"), tok("tokensOut")) else { return answer(400, json!({ "error": "usd is the answer's cost in US dollars (a number from 0), tokensIn and tokensOut whole numbers." })) };
            if pair.is_empty() || !matches!(kind.as_str(), "team" | "mine") { return answer(400, json!({ "error": "A charge names the pair and its kind (\"team\" or \"mine\", as route said)." })); }
            let (p, routed) = pgm_ai_internal::openrouter::pgm_charge_provider(&*ah, &u, &pair, kind == "mine");
            let model = if model.is_empty() { routed.clone() } else { model.chars().take(120).collect() };
            let native = json!({ "prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout, "cost": usd });
            pgm_ai_internal::gateway::charge_external(&ah, &u, &key(), &p, &routed, &model, &native);
            audit::record(&u, "pgm.charge", &pair, json!({ "kind": kind, "usd": usd, "tokensIn": tin, "tokensOut": tout, "model": model }), &ip);
            answer(200, json!({ "ok": true }))
        }
        "sessions/report" if post => {
            let u = sv("user").to_lowercase();
            let Some(a) = acct(&u) else { return no_user(&u) };
            let out = pgm_ai_internal::sessions::report_for(&a, &b, SOURCE);
            audit::record(&u, "pgm.sessions", &u, json!({ "sessions": b["sessions"].as_array().map(|x| x.len()).unwrap_or(0), "status": out.status().as_u16() }), &ip);
            Routed::Answer(out)
        }
        _ => answer(404, json!({ "error": "Not found" })),
    }
}
/// The key the server's answers are counted on (its own line in the key's day roll-up, like a Work run's relay).
fn key() -> pgm_ai_internal::openai::KeyCtx { pgm_ai_internal::openai::KeyCtx { id: "pgm-server".into(), label: "Jcode PGM Server".into(), named: true, ..Default::default() } }

/// `/api/check`'s `pgmServer`: `{ url: "<hub origin>/pgm", signKey }` when the server is set up and its sign key is known
/// (its state file's `signKey`, else config `pgmServer.signKey`); None otherwise (older clients ignore it anyway).
pub fn check_field(hub: &Hub) -> Option<Value> {
    let c = conf(&hub.root);
    server_url(&c)?.ok()?;
    let sf = c["stateFile"].as_str().unwrap_or("").trim();
    let from_state = (!sf.is_empty()).then(|| { let p = PathBuf::from(sf); if p.is_absolute() { p } else { hub.root.join(p) } })
        .and_then(|p| pgm_core_internal::json::read_json(&p)).and_then(|v| v["signKey"].as_str().map(|k| k.trim().to_string())).filter(|k| key_ok(k));
    let sign = from_state.or_else(|| c["signKey"].as_str().map(|k| k.trim().to_string()).filter(|k| key_ok(k)))?;
    Some(json!({ "url": format!("{}/pgm", hub.mods.public_url(&hub.cfg).trim_end_matches('/')), "signKey": sign }))
}
/// A public key as text (base64 / base64url / hex): short, printable, nothing else.
fn key_ok(k: &str) -> bool { (16..=200).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_+/=".contains(&b)) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_servers_address_is_on_this_machine_or_refused() {
        assert_eq!(server_url(&json!({ "url": "http://127.0.0.1:7790/" })), Some(Ok("http://127.0.0.1:7790".to_string())));
        assert!(matches!(server_url(&json!({ "url": "https://example.org:7790" })), Some(Err(_))));
        assert!(matches!(server_url(&json!({ "url": "http://127.0.0.1:7790/x" })), Some(Err(_))));
        assert_eq!(server_url(&json!({})), None);
        assert_eq!(server_url(&Value::Null), None);
        assert!(key_ok("MCowBQYDK2VwAyEAabcdefghijklmnop") && !key_ok("short") && !key_ok("has space in it here ok"));
    }
}
