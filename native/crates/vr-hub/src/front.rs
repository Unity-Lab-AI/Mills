//! The front: one HTTP/1.1 server; each request goes to a native module when one claims it, else to the JavaScript hub.
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::modules::{self, Config};

pub type Body = BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;
pub fn full(b: impl Into<Bytes>) -> Body { Full::new(b.into()).map_err(|e| match e {}).boxed() }
pub fn json(status: u16, v: &serde_json::Value) -> Response<Body> {
    Response::builder().status(status).header("content-type", "application/json").header("cache-control", "no-store").body(full(v.to_string())).expect("response")
}
/// A request as the modules see it: method, path, query, headers, the whole body (read once), the caller's address.
pub struct Req { pub method: hyper::Method, pub path: String, pub query: String, pub headers: hyper::HeaderMap, pub body: Bytes, pub peer: SocketAddr }
impl Req {
    pub fn header(&self, k: &str) -> Option<&str> { self.headers.get(k).and_then(|v| v.to_str().ok()) }
    pub fn json(&self) -> serde_json::Value { serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null) }
    pub fn param(&self, k: &str) -> Option<String> { url_params(&self.query).into_iter().find(|(a, _)| a == k).map(|(_, v)| v) }
}
pub fn url_params(q: &str) -> Vec<(String, String)> {
    q.split('&').filter(|s| !s.is_empty()).map(|kv| { let (k, v) = kv.split_once('=').unwrap_or((kv, "")); (decode(k), decode(v)) }).collect()
}
fn decode(s: &str) -> String {
    let b = s.replace('+', " ").into_bytes(); let mut out = Vec::with_capacity(b.len()); let mut i = 0;
    while i < b.len() { if b[i] == b'%' && i + 2 < b.len() { if let Ok(n) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) { out.push(n); i += 3; continue; } } out.push(b[i]); i += 1; }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Hub {
    pub root: PathBuf,
    pub cfg: Config,
    back: SocketAddr,
    client: Client<HttpConnector, Full<Bytes>>,
    pub mods: modules::Modules,
}
impl Hub {
    pub fn new(root: PathBuf, cfg: Config, back: SocketAddr) -> Result<Arc<Hub>, String> {
        let mut conn = HttpConnector::new(); conn.set_nodelay(true); conn.set_connect_timeout(Some(Duration::from_secs(2)));
        let client = Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(30)).build(conn);
        let mods = modules::Modules::new(&root, &cfg)?;
        Ok(Arc::new(Hub { root, cfg, back, client, mods }))
    }
}

pub async fn serve(hub: Arc<Hub>, addr: SocketAddr) -> i32 {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => { eprintln!("[vr-hub] port {} is taken: another hub runs here", addr.port()); return 2; }
        Err(e) => { eprintln!("[vr-hub] cannot listen on {addr}: {e}"); return 1; }
    };
    println!("[vr-hub] {} in front of the JavaScript hub: http://{addr} → http://{}", crate::VERSION, hub.back);
    modules::start_background(&hub);
    let stop = shutdown();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => { println!("[vr-hub] stopping"); return 0; }
            a = listener.accept() => {
                let Ok((stream, peer)) = a else { continue };
                let hub = hub.clone();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(move |req| { let hub = hub.clone(); async move { Ok::<_, std::convert::Infallible>(handle(hub, req, peer).await) } });
                    let _ = hyper::server::conn::http1::Builder::new().keep_alive(true).serve_connection(TokioIo::new(stream), svc).await;
                });
            }
        }
    }
}
async fn shutdown() {
    #[cfg(unix)]
    { use tokio::signal::unix::{signal, SignalKind}; let mut t = signal(SignalKind::terminate()).expect("signal"); tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = t.recv() => {} } }
    #[cfg(not(unix))]
    { let _ = tokio::signal::ctrl_c().await; }
}

const MAX_BODY: usize = 32 << 20; // the hub takes files up to 24 MB (+ encryption); anything bigger is refused here
const HOP: &[&str] = &["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te", "trailer", "transfer-encoding", "upgrade"];

async fn handle(hub: Arc<Hub>, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let body = match Limited::new(body, MAX_BODY).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return with_front(json(413, &serde_json::json!({ "error": "That is too large." }))),
    };
    let r = Req { method: parts.method.clone(), path: parts.uri.path().to_string(), query: parts.uri.query().unwrap_or("").to_string(), headers: parts.headers.clone(), body, peer };
    // the page's own origins only (bridge/server.js does the same for what it answers): another site's page gets 403;
    // the APIs with their own keys (/v1, /node, /llm) and the internal ones are not for pages
    let cors = if ["/internal/", "/node/", "/v1", "/llm/", "/__front/"].iter().any(|x| r.path.starts_with(x)) { None } else { r.header("origin").map(String::from) };
    if let Some(o) = &cors {
        if !hub.allowed_origin(o) { return with_front(json(403, &serde_json::json!({ "error": "Origin not allowed" }))); }
        if r.method == hyper::Method::OPTIONS { let mut resp = Response::builder().status(204).body(full("")).expect("response"); add_cors(&mut resp, o); return with_front(resp); }
    }
    // a native module answers, or lets the request through (possibly with headers of its own, e.g. who is signed in)
    let mut extra: Vec<(String, String)> = vec![];
    match modules::route(&hub, &r).await {
        modules::Routed::Answer(mut resp) => {
            if let Some(o) = &cors { add_cors(&mut resp, o); }
            // signed-in pages notice a new build on their next call (and every 5 minutes) and update themselves
            if r.path.starts_with("/api/") && !resp.headers().contains_key("x-vr-version") { if let Ok(v) = hyper::header::HeaderValue::from_str(&hub.mods.build.get(&hub).0) { resp.headers_mut().insert("x-vr-version", v); } }
            return with_front(resp);
        }
        modules::Routed::Pass(h) => extra = h,
        modules::Routed::SignedIn { token, user, client, ip } => {
            // Unity learns where they are and greets them; the page gets its token
            crate::modules::unity_routes::signed_in(&hub, &user, client, ip);
            let mut resp = json(200, &serde_json::json!({ "token": token, "username": user }));
            if let Some(o) = &cors { add_cors(&mut resp, o); }
            return with_front(resp);
        }
        modules::Routed::No => {}
    }
    let resp = proxy(&hub, parts, r.body.clone(), extra).await;
    if !modules::wants_after(&r) { return with_front(resp); }
    // a module's part of this answer: the whole answer is read, maybe changed, and handed on
    let (mut p, b) = resp.into_parts();
    let bytes = b.collect().await.map(|c| c.to_bytes()).unwrap_or_default();
    let out = modules::after(&hub, &r, p.status.as_u16(), Some(&bytes)).map(Bytes::from).unwrap_or(bytes);
    p.headers.remove("content-length");
    with_front(Response::from_parts(p, full(out)))
}
/// A word to the JavaScript hub from the front (its internal API, with the front's secret).
pub async fn internal(hub: &Hub, path: &str, body: String) -> Option<(u16, Bytes)> {
    let req = Request::builder().method("POST").uri(format!("http://{}{}", hub.back, path)).header("content-type", "application/json").header("x-vr-front-secret", hub.mods.front_secret.as_str()).body(Full::new(Bytes::from(body))).ok()?;
    let r = tokio::time::timeout(Duration::from_secs(10), hub.client.request(req)).await.ok()?.ok()?;
    let s = r.status().as_u16();
    Some((s, r.into_body().collect().await.ok()?.to_bytes()))
}
/// Every answer: who answered, and the hardening the JavaScript hub puts on its own (no sniffing, no referrer, never framed).
fn with_front(mut r: Response<Body>) -> Response<Body> {
    let h = r.headers_mut();
    h.insert("x-vr-front", hyper::header::HeaderValue::from_static(crate::VERSION));
    for (k, v) in [("x-content-type-options", "nosniff"), ("referrer-policy", "no-referrer"), ("x-frame-options", "DENY")] { if !h.contains_key(k) { h.insert(k, hyper::header::HeaderValue::from_static(v)); } }
    r
}
fn add_cors(r: &mut Response<Body>, origin: &str) {
    let h = r.headers_mut();
    if let Ok(v) = hyper::header::HeaderValue::from_str(origin) { h.insert("access-control-allow-origin", v); }
    h.insert("vary", hyper::header::HeaderValue::from_static("Origin"));
    h.insert("access-control-allow-headers", hyper::header::HeaderValue::from_static("Content-Type, Authorization"));
    h.insert("access-control-allow-methods", hyper::header::HeaderValue::from_static("GET, POST, OPTIONS"));
}
impl Hub {
    /// config allowedOrigins, this machine's own addresses, and config publicUrl (bridge/server.js).
    pub fn allowed_origin(&self, o: &str) -> bool {
        let list = self.cfg.get("allowedOrigins").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_else(|| vec!["https://unity-lab-ai.github.io".into()]);
        let port = self.cfg.port;
        let public = self.cfg.get("publicUrl").and_then(|v| v.as_str()).map(|s| s.trim_end_matches('/').to_lowercase()).filter(|s| s.starts_with("https://")).unwrap_or_default();
        list.iter().any(|x| x == o) || o == format!("http://localhost:{port}") || o == format!("http://127.0.0.1:{port}") || (!public.is_empty() && o.to_lowercase() == public)
    }
}

/// To the JavaScript hub, streamed back. While it is down (a restart), the request waits up to 30 s for it.
async fn proxy(hub: &Hub, parts: hyper::http::request::Parts, body: Bytes, extra: Vec<(String, String)>) -> Response<Body> {
    let pq = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    let started = Instant::now();
    loop {
        let mut b = Request::builder().method(parts.method.clone()).uri(format!("http://{}{}", hub.back, pq));
        // who is signed in is the front's word only: a caller's own x-vr-user / x-vr-front-* never reach the JavaScript hub
        for (k, v) in &parts.headers { if !HOP.contains(&k.as_str()) && !k.as_str().starts_with("x-vr-front-") && k.as_str() != "x-vr-user" { b = b.header(k, v); } }
        for (k, v) in &extra { b = b.header(k.as_str(), v.as_str()); }
        // the front's own word to the JavaScript hub (who is signed in …) is trusted only with the shared secret
        b = b.header("x-vr-front-secret", hub.mods.front_secret.as_str());
        let req = b.body(Full::new(body.clone())).expect("request");
        match hub.client.request(req).await {
            Ok(resp) => {
                let (mut p, inc) = resp.into_parts();
                for h in HOP { p.headers.remove(*h); }
                return Response::from_parts(p, inc.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>).boxed());
            }
            Err(e) if e.is_connect() && started.elapsed() < Duration::from_secs(30) => { tokio::time::sleep(Duration::from_millis(250)).await; }
            Err(e) => return json(if e.is_connect() { 503 } else { 502 }, &serde_json::json!({ "error": "The hub is restarting; try again in a moment.", "detail": e.to_string() })),
        }
    }
}

#[allow(dead_code)]
pub fn status(code: u16) -> StatusCode { StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR) }

#[cfg(test)]
mod tests {
    #[test]
    fn params() {
        let p = super::url_params("a=1&b=x%20y&c=%E2%9C%93&d");
        assert_eq!(p, vec![("a".into(), "1".into()), ("b".into(), "x y".into()), ("c".into(), "✓".into()), ("d".into(), "".into())]);
    }
}
