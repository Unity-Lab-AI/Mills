//! A client for the hub's HTTP API (bridge/server.js), for the native apps: sign in, then any /api route with the
//! session token, plus typed helpers for the routes the apps use most. The client itself (JSON, token, errors, bytes)
//! is pgm-net-internal's ApiClient since 5.0.0; what is VibeRoller's (its routes) is here.
use serde_json::{json, Value};

use crate::{Error, Result};
pub use pgm_net_internal::client::http_client;
use pgm_net_internal::client::ApiClient;

/// The hub's API: everything an ApiClient does (get / post / bytes, `base`, `token`), plus the hub's own routes.
pub struct HubClient(ApiClient);
impl std::ops::Deref for HubClient { type Target = ApiClient; fn deref(&self) -> &ApiClient { &self.0 } }
impl std::ops::DerefMut for HubClient { fn deref_mut(&mut self) -> &mut ApiClient { &mut self.0 } }
impl HubClient {
    pub fn new(base: &str) -> Result<Self> { Ok(HubClient(ApiClient::new(base)?)) }
    pub fn with_token(base: &str, token: &str) -> Result<Self> { Ok(HubClient(ApiClient::with_token(base, token)?)) }

    /// Is a VibeRoller hub there? (anonymous)
    pub async fn status(&self) -> Result<Value> { self.get("/api/status").await }
    /// Signs in; keeps the token. `device_id`: this machine's own stable id (an account is locked to its devices).
    pub async fn login(&mut self, username: &str, password: &str, device_id: &str) -> Result<String> {
        let j: Value = self.post("/api/login", &json!({ "username": username, "password": password, "deviceId": device_id, "client": { "app": "native" } })).await?;
        let t = j.get("token").and_then(Value::as_str).ok_or_else(|| Error::msg("no token in the answer"))?.to_string();
        self.token = Some(t.clone());
        Ok(t)
    }
    pub async fn logout(&mut self) -> Result<()> { let _: Value = self.post("/api/logout", &json!({})).await?; self.token = None; Ok(()) }
    /// Who am I, the models, the projects, the version (/api/check).
    pub async fn check(&self) -> Result<Value> { self.get("/api/check").await }
    /// The build the hub runs ({ version, build }).
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
    /// What the Download panel has (signed in; the public /api/pub/versions is gone since 4.0.0).
    pub async fn downloads(&self) -> Result<Value> { self.get("/api/downloads").await }
}

/// encodeURIComponent (pgm-core-internal since 5.0.0)
pub use pgm_core_internal::text::urlencode;
