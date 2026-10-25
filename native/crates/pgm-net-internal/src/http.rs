//! The HTTP server kit: one request read whole (with a size cap), JSON / text / streamed answers, query parameters as
//! the browser encodes them, CORS for the page's own origins, the hardening headers every answer carries, and a
//! reverse proxy to a backend that waits through the backend's restarts.
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Every answer's body: whole bytes or a stream.
pub type Body = BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;
pub fn full(b: impl Into<Bytes>) -> Body { Full::new(b.into()).map_err(|e| match e {}).boxed() }
/// A JSON answer, never cached.
pub fn json(status: u16, v: &serde_json::Value) -> Response<Body> {
    Response::builder().status(status).header("content-type", "application/json").header("cache-control", "no-store").body(full(v.to_string())).expect("response")
}

/// A request as handlers see it: method, path, query, headers, the whole body (read once), the caller's address.
pub struct Req { pub method: hyper::Method, pub path: String, pub query: String, pub headers: hyper::HeaderMap, pub body: Bytes, pub peer: SocketAddr }
impl Req {
    pub fn header(&self, k: &str) -> Option<&str> { self.headers.get(k).and_then(|v| v.to_str().ok()) }
    pub fn json(&self) -> serde_json::Value { serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null) }
    pub fn param(&self, k: &str) -> Option<String> { url_params(&self.query).into_iter().find(|(a, _)| a == k).map(|(_, v)| v) }
}
/// Reads a request whole (at most `max` bytes): the request and its head (for a proxy), or a 413 answer.
pub async fn read(req: Request<Incoming>, peer: SocketAddr, max: usize) -> Result<(Req, hyper::http::request::Parts), Response<Body>> {
    let (parts, body) = req.into_parts();
    let body = match Limited::new(body, max).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Err(json(413, &serde_json::json!({ "error": "That is too large." }))),
    };
    Ok((Req { method: parts.method.clone(), path: parts.uri.path().to_string(), query: parts.uri.query().unwrap_or("").to_string(), headers: parts.headers.clone(), body, peer }, parts))
}
/// `a=1&b=x%20y` → [("a","1"), ("b","x y")] (+ is a space, as forms send it).
pub fn url_params(q: &str) -> Vec<(String, String)> {
    q.split('&').filter(|s| !s.is_empty()).map(|kv| { let (k, v) = kv.split_once('=').unwrap_or((kv, "")); (decode(k), decode(v)) }).collect()
}
fn decode(s: &str) -> String {
    let b = s.replace('+', " ").into_bytes(); let mut out = Vec::with_capacity(b.len()); let mut i = 0;
    while i < b.len() { if b[i] == b'%' && i + 2 < b.len() { if let Ok(n) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) { out.push(n); i += 3; continue; } } out.push(b[i]); i += 1; }
    String::from_utf8_lossy(&out).into_owned()
}

/// The hardening every answer carries (unless a handler set its own): no sniffing, no referrer, never framed; and
/// `server` = (header name, value) naming who answered.
pub fn harden(mut r: Response<Body>, server: (&'static str, &'static str)) -> Response<Body> {
    let h = r.headers_mut();
    h.insert(server.0, hyper::header::HeaderValue::from_static(server.1));
    for (k, v) in [("x-content-type-options", "nosniff"), ("referrer-policy", "no-referrer"), ("x-frame-options", "DENY")] { if !h.contains_key(k) { h.insert(k, hyper::header::HeaderValue::from_static(v)); } }
    r
}
/// CORS for an allowed origin: that origin, GET / POST / OPTIONS, Content-Type + Authorization.
pub fn add_cors(r: &mut Response<Body>, origin: &str) {
    let h = r.headers_mut();
    if let Ok(v) = hyper::header::HeaderValue::from_str(origin) { h.insert("access-control-allow-origin", v); }
    h.insert("vary", hyper::header::HeaderValue::from_static("Origin"));
    h.insert("access-control-allow-headers", hyper::header::HeaderValue::from_static("Content-Type, Authorization"));
    h.insert("access-control-allow-methods", hyper::header::HeaderValue::from_static("GET, POST, OPTIONS"));
}
/// May a browser keep this answer and hand it back later (cache-control with a max-age above 0, no no-store / no-cache)?
/// What such an answer says about the server (its build) can be stale by the time it is read again.
pub fn cacheable(r: &Response<Body>) -> bool {
    let cc = r.headers().get("cache-control").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    !cc.contains("no-store") && !cc.contains("no-cache") && cc.split(',').filter_map(|d| d.trim().strip_prefix("max-age=")).any(|n| n.trim().parse::<u64>().map(|n| n > 0).unwrap_or(false))
}
/// The CORS preflight answer (204) for an allowed origin.
pub fn preflight(origin: &str) -> Response<Body> { let mut resp = Response::builder().status(204).body(full("")).expect("response"); add_cors(&mut resp, origin); resp }
/// Is `o` an allowed origin: one of `list`, this machine on `port` (localhost / 127.0.0.1), or the public https address.
pub fn origin_allowed(o: &str, list: &[String], port: u16, public_url: &str) -> bool {
    let public = public_url.trim_end_matches('/').to_lowercase();
    let public = if public.starts_with("https://") { public } else { String::new() };
    list.iter().any(|x| x == o) || o == format!("http://localhost:{port}") || o == format!("http://127.0.0.1:{port}") || (!public.is_empty() && o.to_lowercase() == public)
}

/// The headers that belong to one connection only (never passed on by a proxy).
pub const HOP: &[&str] = &["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te", "trailer", "transfer-encoding", "upgrade"];

/// A backend behind this server (on this machine): requests passed on with a shared secret header, streamed back.
pub struct Backend {
    pub addr: SocketAddr,
    /// (header, value) the backend trusts this server by; an empty header name = none
    pub secret: (String, String),
    /// how long a request waits for a backend that is down (a restart) before answering 503
    pub wait: Duration,
    client: Client<HttpConnector, Full<Bytes>>,
}
impl Backend {
    pub fn new(addr: SocketAddr, secret_header: &str, secret: &str) -> Backend {
        let mut conn = HttpConnector::new(); conn.set_nodelay(true); conn.set_connect_timeout(Some(Duration::from_secs(2)));
        let client = Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(30)).build(conn);
        Backend { addr, secret: (secret_header.to_string(), secret.to_string()), wait: Duration::from_secs(30), client }
    }
    /// A backend with no shared secret (a program of its own on this machine, e.g. a product behind a gateway), waited
    /// for `wait` while it is down.
    pub fn plain(addr: SocketAddr, wait: Duration) -> Backend { let mut b = Backend::new(addr, "", ""); b.wait = wait; b }
    /// Passes a request on, streamed back. `keep(header)` says which of the caller's headers go along (the hop headers
    /// never do); `extra` are this server's own. While the backend is down (a restart) it waits up to `wait` for it.
    pub async fn proxy(&self, parts: &hyper::http::request::Parts, body: Bytes, keep: impl Fn(&str) -> bool, extra: &[(String, String)]) -> Response<Body> {
        let pq = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
        self.proxy_to(&pq, parts.method.clone(), &parts.headers, body, keep, extra).await
    }
    /// As [`proxy`](Self::proxy), to a path of this server's choosing (a prefix taken off, say).
    pub async fn proxy_to(&self, pq: &str, method: hyper::Method, headers: &hyper::HeaderMap, body: Bytes, keep: impl Fn(&str) -> bool, extra: &[(String, String)]) -> Response<Body> {
        let started = Instant::now();
        loop {
            let mut b = Request::builder().method(method.clone()).uri(format!("http://{}{}", self.addr, pq));
            for (k, v) in headers { if !HOP.contains(&k.as_str()) && keep(k.as_str()) { b = b.header(k, v); } }
            for (k, v) in extra { b = b.header(k.as_str(), v.as_str()); }
            if !self.secret.0.is_empty() { b = b.header(self.secret.0.as_str(), self.secret.1.as_str()); }
            let req = b.body(Full::new(body.clone())).expect("request");
            match self.client.request(req).await {
                Ok(resp) => {
                    let (mut p, inc) = resp.into_parts();
                    for h in HOP { p.headers.remove(*h); }
                    return Response::from_parts(p, inc.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>).boxed());
                }
                Err(e) if e.is_connect() && started.elapsed() < self.wait => { tokio::time::sleep(Duration::from_millis(250)).await; }
                Err(e) => return json(if e.is_connect() { 503 } else { 502 }, &serde_json::json!({ "error": "The hub is restarting; try again in a moment.", "detail": e.to_string() })),
            }
        }
    }
    /// A POST of JSON to the backend's own internal route (with the secret): (status, body), or None (down, 10 s).
    pub async fn internal(&self, path: &str, body: String) -> Option<(u16, Bytes)> {
        let req = Request::builder().method("POST").uri(format!("http://{}{}", self.addr, path)).header("content-type", "application/json").header(self.secret.0.as_str(), self.secret.1.as_str()).body(Full::new(Bytes::from(body))).ok()?;
        let r = tokio::time::timeout(Duration::from_secs(10), self.client.request(req)).await.ok()?.ok()?;
        let s = r.status().as_u16();
        Some((s, r.into_body().collect().await.ok()?.to_bytes()))
    }
}

/// Resolves when the program is asked to stop (Ctrl+C; SIGTERM on Unix).
pub async fn shutdown_signal() {
    #[cfg(unix)]
    { use tokio::signal::unix::{signal, SignalKind}; let mut t = signal(SignalKind::terminate()).expect("signal"); tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = t.recv() => {} } }
    #[cfg(not(unix))]
    { let _ = tokio::signal::ctrl_c().await; }
}

#[cfg(test)]
mod tests {
    #[test]
    fn params_and_origins() {
        let p = super::url_params("a=1&b=x%20y&c=%E2%9C%93&d");
        assert_eq!(p, vec![("a".into(), "1".into()), ("b".into(), "x y".into()), ("c".into(), "✓".into()), ("d".into(), "".into())]);
        let list = vec!["https://unity-lab-ai.github.io".to_string()];
        assert!(super::origin_allowed("https://unity-lab-ai.github.io", &list, 7777, ""));
        assert!(super::origin_allowed("http://127.0.0.1:7777", &list, 7777, ""));
        assert!(super::origin_allowed("https://Portal.Example.org", &list, 7777, "https://portal.example.org/"));
        assert!(!super::origin_allowed("https://evil.example", &list, 7777, "https://portal.example.org"));
    }
}
