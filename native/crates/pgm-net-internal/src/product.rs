//! The product link: a product (its own program, data and accounts) asking a PGM Gateway on the same machine for what
//! only the gateway has (the identity directory, the AI hosts). Every request is signed with the product's secret:
//!
//! `x-pgm-product: <id>`, `x-pgm-ts: <ms>`, `x-pgm-nonce: <hex>`, `x-pgm-user: <account or "">`,
//! `x-pgm-sig: hex HMAC-SHA256(secret, "pgm-product-v1\n{METHOD}\n{path?query}\n{ts}\n{nonce}\n{user}\n{sha256(body)}")`
//!
//! The gateway takes a request only from 127.0.0.1, within [`MAX_SKEW_MS`] of its own clock, with a nonce it has not
//! seen, and with the signature of that product's secret: nothing of it can be replayed, moved to another path or
//! another person, or changed on the way.
use pgm_core_internal::crypto::{hmac_hex, hmac_ok, random_hex, sha256_hex};
use pgm_core_internal::now_ms;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::http::Req;

/// How far a request's time may be from the gateway's.
pub const MAX_SKEW_MS: u64 = 60_000;

fn payload(method: &str, path_q: &str, ts: u64, nonce: &str, user: &str, body: &[u8]) -> String {
    format!("pgm-product-v1\n{}\n{path_q}\n{ts}\n{nonce}\n{user}\n{}", method.to_ascii_uppercase(), sha256_hex(body))
}
/// The signature of one request.
pub fn sign(secret: &str, method: &str, path_q: &str, ts: u64, nonce: &str, user: &str, body: &[u8]) -> String {
    hmac_hex(secret.as_bytes(), payload(method, path_q, ts, nonce, user, body).as_bytes())
}
/// The five headers of a signed request.
pub fn headers(product: &str, secret: &str, method: &str, path_q: &str, user: &str, body: &[u8]) -> Vec<(&'static str, String)> {
    let (ts, nonce) = (now_ms(), random_hex(16));
    let sig = sign(secret, method, path_q, ts, &nonce, user, body);
    vec![("x-pgm-product", product.to_string()), ("x-pgm-ts", ts.to_string()), ("x-pgm-nonce", nonce), ("x-pgm-user", user.to_string()), ("x-pgm-sig", sig)]
}
/// A product id: 1–32 of a-z, 0-9 and "-".
pub fn valid_id(id: &str) -> bool { !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') }

/// Who a signed request is from.
#[derive(Debug, Clone, PartialEq)]
pub struct Signed { pub product: String, pub user: String }

/// The gateway's side: checks requests and remembers their nonces (for twice [`MAX_SKEW_MS`]).
#[derive(Default)]
pub struct Verifier { seen: Mutex<HashMap<String, u64>> }
impl Verifier {
    /// `secret_of`: a product's secret by its id (None: no such product).
    pub fn check(&self, r: &Req, secret_of: &dyn Fn(&str) -> Option<String>) -> Result<Signed, &'static str> {
        // a reverse proxy on this machine (nginx) connects from 127.0.0.1 too: what it passed on is from outside
        let proxied = ["x-real-ip", "x-forwarded-for", "forwarded", "cf-connecting-ip"].iter().any(|k| r.headers.contains_key(*k));
        if !r.peer.ip().is_loopback() || proxied { return Err("only from this machine"); }
        let h = |k: &str| r.header(k).unwrap_or("").to_string();
        let (product, user, nonce, sig) = (h("x-pgm-product"), h("x-pgm-user"), h("x-pgm-nonce"), h("x-pgm-sig"));
        let ts: u64 = h("x-pgm-ts").parse().map_err(|_| "unsigned")?;
        if !valid_id(&product) || nonce.len() < 16 || nonce.len() > 64 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) || sig.len() != 64 { return Err("unsigned"); }
        if user.len() > 64 || user.bytes().any(|b| b.is_ascii_control()) { return Err("bad user"); }
        let now = now_ms();
        if ts.abs_diff(now) > MAX_SKEW_MS { return Err("too old"); }
        let secret = secret_of(&product).ok_or("unknown product")?;
        let path_q = if r.query.is_empty() { r.path.clone() } else { format!("{}?{}", r.path, r.query) };
        if !hmac_ok(secret.as_bytes(), payload(r.method.as_str(), &path_q, ts, &nonce, &user, &r.body).as_bytes(), &sig) { return Err("bad signature"); }
        let mut seen = self.seen.lock().unwrap();
        seen.retain(|_, t| now.saturating_sub(*t) <= 2 * MAX_SKEW_MS);
        if seen.insert(format!("{product}|{nonce}"), now).is_some() { return Err("replayed"); }
        Ok(Signed { product, user })
    }
}

/// The product's side: a client for its gateway (`base`: e.g. http://127.0.0.1:7777).
pub struct GatewayClient { pub base: String, pub product: String, secret: String, http: reqwest::Client }
impl GatewayClient {
    pub fn new(base: &str, product: &str, secret: &str) -> Result<GatewayClient, String> {
        Ok(GatewayClient { base: base.trim_end_matches('/').to_string(), product: product.to_string(), secret: secret.trim().to_string(), http: crate::client::http_client(Duration::from_secs(600)).map_err(|e| e.to_string())? })
    }
    /// A signed request as `user` ("" for the product itself); `path_q` starts with /internal/gateway/.
    pub fn request(&self, method: reqwest::Method, path_q: &str, user: &str, body: Vec<u8>) -> reqwest::RequestBuilder {
        let mut rb = self.http.request(method.clone(), format!("{}{path_q}", self.base));
        for (k, v) in headers(&self.product, &self.secret, method.as_str(), path_q, user, &body) { rb = rb.header(k, v); }
        if !body.is_empty() { rb = rb.header("content-type", "application/json"); }
        rb.body(body)
    }
    /// A signed JSON call: (status, answer).
    pub async fn call(&self, method: reqwest::Method, path_q: &str, user: &str, body: Option<&serde_json::Value>) -> Result<(u16, serde_json::Value), String> {
        let r = self.request(method, path_q, user, body.map(|b| b.to_string().into_bytes()).unwrap_or_default()).send().await.map_err(|e| e.to_string())?;
        let st = r.status().as_u16();
        Ok((st, r.json().await.unwrap_or(serde_json::Value::Null)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn req(hs: &[(&'static str, String)], path: &str, body: &[u8], peer: &str) -> Req {
        let mut headers = hyper::HeaderMap::new();
        for (k, v) in hs { headers.insert(*k, v.parse().unwrap()); }
        Req { method: hyper::Method::POST, path: path.into(), query: String::new(), headers, body: bytes::Bytes::copy_from_slice(body), peer: peer.parse().unwrap() }
    }
    #[test]
    fn signed_requests() {
        let v = Verifier::default();
        let sec = |p: &str| (p == "prodsuit").then(|| "s3cret".to_string());
        let hs = headers("prodsuit", "s3cret", "POST", "/internal/gateway/x", "sponge", b"{}");
        let r = req(&hs, "/internal/gateway/x", b"{}", "127.0.0.1:5");
        assert_eq!(v.check(&r, &sec), Ok(Signed { product: "prodsuit".into(), user: "sponge".into() }));
        assert_eq!(v.check(&r, &sec), Err("replayed"));
        let hs = headers("prodsuit", "s3cret", "POST", "/internal/gateway/x", "sponge", b"{}");
        assert_eq!(v.check(&req(&hs, "/internal/gateway/x", b"{\"a\":1}", "127.0.0.1:5"), &sec), Err("bad signature"));
        assert_eq!(v.check(&req(&hs, "/internal/gateway/y", b"{}", "127.0.0.1:5"), &sec), Err("bad signature"));
        assert_eq!(v.check(&req(&hs, "/internal/gateway/x", b"{}", "10.0.0.2:5"), &sec), Err("only from this machine"));
        let mut via = hs.clone(); via.push(("x-forwarded-for", "8.8.8.8".into()));
        assert_eq!(v.check(&req(&via, "/internal/gateway/x", b"{}", "127.0.0.1:5"), &sec), Err("only from this machine"));
        let other = headers("prodsuit", "wrong", "POST", "/internal/gateway/x", "sponge", b"{}");
        assert_eq!(v.check(&req(&other, "/internal/gateway/x", b"{}", "127.0.0.1:5"), &sec), Err("bad signature"));
        let unknown = headers("nope", "s3cret", "POST", "/internal/gateway/x", "", b"");
        assert_eq!(v.check(&req(&unknown, "/internal/gateway/x", b"", "127.0.0.1:5"), &sec), Err("unknown product"));
        assert_eq!(v.check(&req(&[], "/internal/gateway/x", b"", "127.0.0.1:5"), &sec), Err("unsigned"));
    }
}
