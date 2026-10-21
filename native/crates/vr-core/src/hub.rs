//! A client for the hub's HTTP API (bridge/server.js), for the native apps: sign in, then any /api route with the
//! session token, plus typed helpers for the routes the apps use most.
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Once;
use std::time::Duration;

use crate::{Error, Result};

static TLS: Once = Once::new();
/// The HTTP client every VibeRoller program uses: rustls with the ring provider (no system OpenSSL, the same on Linux
/// and Windows), HTTP/2, a connect timeout, and our own user agent.
pub fn http_client(connect: Duration) -> Result<reqwest::Client> {
    TLS.call_once(|| { let _ = rustls::crypto::ring::default_provider().install_default(); });
    Ok(reqwest::Client::builder().connect_timeout(connect).user_agent(concat!("VibeRoller-native/", env!("CARGO_PKG_VERSION"))).build()?)
}

pub struct HubClient { pub base: String, pub token: Option<String>, http: reqwest::Client }
impl HubClient {
    pub fn new(base: &str) -> Result<Self> { Ok(HubClient { base: base.trim_end_matches('/').to_string(), token: None, http: http_client(Duration::from_secs(15))? }) }
    pub fn with_token(base: &str, token: &str) -> Result<Self> { let mut c = Self::new(base)?; c.token = Some(token.to_string()); Ok(c) }
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self.http.request(method, format!("{}{}", self.base, path)).timeout(Duration::from_secs(120));
        match &self.token { Some(t) => rb.bearer_auth(t), None => rb }
    }
    async fn answer<T: DeserializeOwned>(r: reqwest::Response) -> Result<T> {
        let status = r.status().as_u16();
        let text = r.text().await?;
        if !(200..300).contains(&status) {
            let msg = serde_json::from_str::<Value>(&text).ok().and_then(|j| j.get("error").and_then(|e| e.as_str().map(String::from).or_else(|| e.get("message").and_then(Value::as_str).map(String::from)))).unwrap_or_else(|| format!("the hub answered {status}"));
            return Err(Error::status(status, msg));
        }
        Ok(serde_json::from_str(&text)?)
    }
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> { Self::answer(self.req(reqwest::Method::GET, path).send().await?).await }
    pub async fn post<T: DeserializeOwned, B: Serialize + ?Sized>(&self, path: &str, body: &B) -> Result<T> { Self::answer(self.req(reqwest::Method::POST, path).json(body).send().await?).await }
    /// Raw bytes (a file), with the type the hub names in X-VR-File-Type.
    pub async fn get_bytes(&self, path: &str) -> Result<(Vec<u8>, String)> {
        let r = self.req(reqwest::Method::GET, path).send().await?;
        let status = r.status().as_u16();
        if !(200..300).contains(&status) { return Err(Error::status(status, format!("the hub answered {status}"))); }
        let ty = r.headers().get("x-vr-file-type").and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
        Ok((r.bytes().await?.to_vec(), ty))
    }
    pub async fn post_bytes<T: DeserializeOwned>(&self, path: &str, bytes: Vec<u8>, headers: &[(&str, &str)]) -> Result<T> {
        let mut rb = self.req(reqwest::Method::POST, path).body(bytes);
        for (k, v) in headers { rb = rb.header(*k, *v); }
        Self::answer(rb.send().await?).await
    }

    /// Is a VibeRoller hub there? (anonymous)
    pub async fn status(&self) -> Result<Value> { self.get("/api/status").await }
    /// Signs in; keeps the token. `device_id`: this machine's own stable id (an account is locked to its first device).
    pub async fn login(&mut self, username: &str, password: &str, device_id: &str) -> Result<String> {
        let j: Value = self.post("/api/login", &json!({ "username": username, "password": password, "deviceId": device_id, "client": { "app": "native" } })).await?;
        let t = j.get("token").and_then(Value::as_str).ok_or_else(|| Error::msg("no token in the answer"))?.to_string();
        self.token = Some(t.clone());
        Ok(t)
    }
    pub async fn logout(&mut self) -> Result<()> { let _: Value = self.post("/api/logout", &json!({})).await?; self.token = None; Ok(()) }
    /// Who am I, the models, the projects, the version (/api/check).
    pub async fn check(&self) -> Result<Value> { self.get("/api/check").await }
    /// The build the hub runs ({ version, commit, build }).
    pub async fn version(&self) -> Result<Value> { self.get("/api/version").await }
    /// Files: { linux, entries }.
    pub async fn library(&self) -> Result<Value> { self.get("/api/library").await }
    pub async fn work_file(&self, id: &str) -> Result<(Vec<u8>, String)> { self.get_bytes(&format!("/api/work/file/{id}")).await }
    /// Uploads a file to the person's uploaded_files (as Work's + button does).
    pub async fn upload_work_file(&self, name: &str, mime: &str, bytes: Vec<u8>) -> Result<Value> {
        let enc = urlencode(name);
        self.post_bytes("/api/work/file", bytes, &[("Content-Type", mime), ("X-File-Name", &enc)]).await
    }
    pub async fn board(&self) -> Result<Value> { self.get("/api/board").await }
    /// The published package versions (anonymous): what the Desktop and AI Host updaters compare with.
    /// What the Download panel has (signed in; the public /api/pub/versions is gone since 4.0.0).
    pub async fn downloads(&self) -> Result<Value> { self.get("/api/downloads").await }
}

/// encodeURIComponent
pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) { out.push(b as char) } else { out.push_str(&format!("%{b:02X}")) }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn urlencode_like_the_browser() { assert_eq!(super::urlencode("a b/ü.md"), "a%20b%2F%C3%BC.md"); }
}
