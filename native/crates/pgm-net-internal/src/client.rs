//! HTTP clients.
//! - [`http_client`]: the client every PGM program uses: rustls with the ring provider (no system OpenSSL, the same on
//!   Linux and Windows), HTTP/2, a connect timeout, its own user agent
//! - [`no_redirects`]: the same without following redirects (a call to an address someone configured must not be bent
//!   elsewhere)
//! - [`ApiClient`]: a JSON API with a bearer token: get / post, raw bytes both ways, the answer's error message
use pgm_core_internal::{Error, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::sync::Once;
use std::time::Duration;

static TLS: Once = Once::new();
fn tls() { TLS.call_once(|| { let _ = rustls::crypto::ring::default_provider().install_default(); }); }
/// The HTTP client every PGM program uses.
pub fn http_client(connect: Duration) -> Result<reqwest::Client> {
    tls();
    Ok(reqwest::Client::builder().connect_timeout(connect).user_agent(concat!("VibeRoller-native/", env!("CARGO_PKG_VERSION"))).build()?)
}
/// The same, following no redirects.
pub fn no_redirects(connect: Duration) -> Result<reqwest::Client> {
    tls();
    Ok(reqwest::Client::builder().connect_timeout(connect).redirect(reqwest::redirect::Policy::none()).user_agent(concat!("VibeRoller-native/", env!("CARGO_PKG_VERSION"))).build()?)
}

/// A JSON API at `base`, signed in with `token` (Bearer) when there is one.
pub struct ApiClient { pub base: String, pub token: Option<String>, http: reqwest::Client }
impl ApiClient {
    pub fn new(base: &str) -> Result<Self> { Ok(ApiClient { base: base.trim_end_matches('/').to_string(), token: None, http: http_client(Duration::from_secs(15))? }) }
    pub fn with_token(base: &str, token: &str) -> Result<Self> { let mut c = Self::new(base)?; c.token = Some(token.to_string()); Ok(c) }
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self.http.request(method, format!("{}{}", self.base, path)).timeout(Duration::from_secs(120));
        match &self.token { Some(t) => rb.bearer_auth(t), None => rb }
    }
    /// A 2xx answer as T; else an error carrying the status and the answer's own message (`error` or `error.message`).
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
    /// Raw bytes (a file), with the type named in X-VR-File-Type.
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
}
