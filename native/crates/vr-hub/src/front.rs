//! The front: one HTTP/1.1 server; each request goes to a native module when one claims it, else to the JavaScript hub.
//! The server kit (requests, answers, CORS, hardening, the reverse proxy) is pgm-net-internal's (PGM_Net_Internal)
//! since 5.0.0; what is the hub's (its modules, its origins, who answers) is here.
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::modules::{self, Config};
pub use pgm_net_internal::http::{full, json, url_params, Body, Req};
use pgm_net_internal::http::{self as kit, Backend};

pub struct Hub {
    pub root: PathBuf,
    pub cfg: Config,
    /// the JavaScript hub behind this one (it trusts this front by the shared secret)
    backend: Backend,
    pub mods: modules::Modules,
    /// the hub itself, for library work that outlives a call (the chat calling an agent)
    me: std::sync::Weak<Hub>,
}
impl Hub {
    pub fn new(root: PathBuf, cfg: Config, back: SocketAddr) -> Result<Arc<Hub>, String> {
        let mods = modules::Modules::new(&root, &cfg)?;
        let backend = Backend::new(back, "x-vr-front-secret", mods.front_secret.as_str());
        Ok(Arc::new_cyclic(|me| Hub { root, cfg, backend, mods, me: me.clone() }))
    }
    pub fn me(&self) -> Option<Arc<dyn pgm_ai_internal::hub::AiHub>> { self.me.upgrade().map(|h| h as Arc<dyn pgm_ai_internal::hub::AiHub>) }
}

pub async fn serve(hub: Arc<Hub>, addr: SocketAddr) -> i32 {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => { eprintln!("[vr-hub] port {} is taken: another hub runs here", addr.port()); return 2; }
        Err(e) => { eprintln!("[vr-hub] cannot listen on {addr}: {e}"); return 1; }
    };
    println!("[vr-hub] {} in front of the JavaScript hub: http://{addr} → http://{}", crate::VERSION, hub.backend.addr);
    modules::start_background(&hub);
    let stop = kit::shutdown_signal();
    tokio::pin!(stop);
    let restart = modules::site::restart_when_asked(hub.clone());
    tokio::pin!(restart);
    loop {
        tokio::select! {
            _ = &mut stop => { println!("[vr-hub] stopping"); return 0; }
            _ = &mut restart => { return 75; } // run.js starts both hubs again on the new code

            a = listener.accept() => {
                let Ok((stream, peer)) = a else { continue };
                let hub = hub.clone();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(move |req| { let hub = hub.clone(); async move { Ok::<_, std::convert::Infallible>(guarded(hub, req, peer).await) } });
                    let _ = hyper::server::conn::http1::Builder::new().keep_alive(true).serve_connection(TokioIo::new(stream), svc).await;
                });
            }
        }
    }
}

const MAX_BODY: usize = 32 << 20; // the hub takes files up to 24 MB (+ encryption); anything bigger is refused here

/// One request's failure is that request's alone (FR1, plans/full-rust-hub.md): a panic inside `handle` answers that
/// request 500 and is logged with its method and path; the hub, its other connections and its locks (every lock
/// recovers from a poisoning) go on. Before 5.4.0 the release build aborted the whole hub on any panic.
async fn guarded(hub: Arc<Hub>, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    use futures_util::FutureExt;
    let (m, p) = (req.method().clone(), req.uri().path().to_string());
    match std::panic::AssertUnwindSafe(handle(hub, req, peer)).catch_unwind().await {
        Ok(r) => r,
        Err(e) => {
            let why = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic".into());
            eprintln!("[vr-hub] a request failed inside the hub: {m} {p}: {why} (answered 500; the hub goes on)");
            let mut r = json(500, &serde_json::json!({ "error": "That request failed inside the hub; it was logged." }));
            r.headers_mut().insert("x-vr-by", hyper::header::HeaderValue::from_static("rust"));
            with_front(r)
        }
    }
}

async fn handle(hub: Arc<Hub>, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    let (r, parts) = match kit::read(req, peer, MAX_BODY).await { Ok(x) => x, Err(too_big) => return with_front(too_big) };
    // the page's own origins only (bridge/server.js does the same for what it answers): another site's page gets 403;
    // the APIs with their own keys (/v1, /node, /llm) and the internal ones are not for pages
    let cors = if ["/internal/", "/node/", "/v1", "/llm/", "/__front/"].iter().any(|x| r.path.starts_with(x)) { None } else { r.header("origin").map(String::from) };
    if let Some(o) = &cors {
        if !hub.allowed_origin(o) { return with_front(json(403, &serde_json::json!({ "error": "Origin not allowed" }))); }
        if r.method == hyper::Method::OPTIONS { return with_front(kit::preflight(o)); }
    }
    // a native module answers, or lets the request through (possibly with headers of its own, e.g. who is signed in)
    let mut extra: Vec<(String, String)> = vec![];
    match modules::route(&hub, &r).await {
        modules::Routed::Answer(mut resp) => {
            if let Some(o) = &cors { kit::add_cors(&mut resp, o); }
            resp.headers_mut().insert("x-vr-by", hyper::header::HeaderValue::from_static("rust")); // who answered (FR6's meter: R179)
            // signed-in pages notice a new build on their next call (and every 5 minutes) and update themselves
            if r.path.starts_with("/api/") && !resp.headers().contains_key("x-vr-version") { if let Ok(v) = hyper::header::HeaderValue::from_str(&hub.mods.build.get(&hub).0) { resp.headers_mut().insert("x-vr-version", v); } }
            return with_front(resp);
        }
        modules::Routed::Pass(h) => extra = h,
        modules::Routed::SignedIn { token, user, client, ip } => {
            // Unity learns where they are and greets them; the page gets its token
            crate::modules::unity_routes::signed_in(&hub, &user, client, ip);
            let mut resp = json(200, &serde_json::json!({ "token": token, "username": user }));
            if let Some(o) = &cors { kit::add_cors(&mut resp, o); }
            resp.headers_mut().insert("x-vr-by", hyper::header::HeaderValue::from_static("rust"));
            return with_front(resp);
        }
        modules::Routed::No => {}
    }
    // who is signed in is the front's word only: a caller's own x-vr-user / x-vr-front-* never reach the JavaScript hub
    let mut resp = hub.backend.proxy(&parts, r.body.clone(), |k| !k.starts_with("x-vr-front-") && k != "x-vr-user", &extra).await;
    resp.headers_mut().insert("x-vr-by", hyper::header::HeaderValue::from_static("node")); // the JavaScript hub answered (what FR6 still has to move: R179)
    // One build per hub: the page compares X-VR-Version with the build /api/check named (this hub's), so an answer of
    // the JavaScript hub behind it must name the same one. Its own (read when IT started) could differ when the two
    // started apart (one restarted alone after a crash, or an older one kept running), and then every page offered an
    // update to that other build, again and again (Sponge saw "VibeRoller 4.1.0 is ready" on 5.x). The page and its
    // files come from here, so this hub's build is the one that counts; a different one is logged once.
    if r.path.starts_with("/api/") {
        let mine = hub.mods.build.get(&hub).0;
        if let Some(theirs) = resp.headers().get("x-vr-version").and_then(|v| v.to_str().ok()).filter(|t| *t != mine) { hub.mods.build.note_back(theirs, &mine); }
        if let Ok(v) = hyper::header::HeaderValue::from_str(&mine) { resp.headers_mut().insert("x-vr-version", v); }
    }
    if !modules::wants_after(&r) { return with_front(resp); }
    // a module's part of this answer: the whole answer is read, maybe changed, and handed on
    let (mut p, b) = resp.into_parts();
    let bytes = b.collect().await.map(|c| c.to_bytes()).unwrap_or_default();
    let out = modules::after(&hub, &r, p.status.as_u16(), Some(&bytes)).map(Bytes::from).unwrap_or(bytes);
    p.headers.remove("content-length");
    with_front(Response::from_parts(p, full(out)))
}
/// A word to the JavaScript hub from the front (its internal API, with the front's secret).
pub async fn internal(hub: &Hub, path: &str, body: String) -> Option<(u16, Bytes)> { hub.backend.internal(path, body).await }
/// Every answer: who answered, and the hardening the JavaScript hub puts on its own (no sniffing, no referrer, never framed).
fn with_front(r: Response<Body>) -> Response<Body> { kit::harden(r, ("x-vr-front", crate::VERSION)) }
impl Hub {
    /// config allowedOrigins, this machine's own addresses, and config publicUrl (bridge/server.js).
    pub fn allowed_origin(&self, o: &str) -> bool {
        let list = self.cfg.get("allowedOrigins").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_else(|| vec!["https://unity-lab-ai.github.io".into()]);
        kit::origin_allowed(o, &list, self.cfg.port, self.cfg.get("publicUrl").and_then(|v| v.as_str()).unwrap_or(""))
    }
}

#[allow(dead_code)]
pub fn status(code: u16) -> StatusCode { StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR) }
