//! The AI host protocol (hub side: bridge/ai-nodes.js; the JavaScript host: bridge/ai-node.js).
//!
//! A host connects OUT to its hub over HTTPS and never opens a port:
//!   POST /node/hello     what it offers + a nonce → its nickname, the hub's nonce, the key id (both sides derive the same
//!                        session key, see [`crate::crypto::HostKey`]), the poll time, the team group, the hub's release
//!   GET  /node/poll      long poll: jobs + cancels + the hub's heartbeat (`beat`: time, version, commit). 428 = say
//!                        hello again (the hub lost the session), 401 = the hub does not know this host
//!   POST /node/reply     a job's whole answer          POST /node/stream   a streamed answer, piece by piece
//!   POST /node/events    a Work run's events (sealed)  GET  /node/update   the signed manifest of the host's files
//! Every call carries X-VR-Host / X-VR-Host-Token (and the 1.x names X-VR-Node / X-VR-Node-Token) and X-VR-Host-Version.
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::time::Duration;

use crate::crypto::{Envelope, HostKey};
use crate::{b64, Error, Result};

/// What a host can do, told to the hub in hello (older hosts send none). "update-bin": it updates its own binary.
pub const CAPS: &[&str] = &["relay", "update", "update-bin"];

/// One provider as hello offers it.
#[derive(Clone, Debug, Serialize)]
pub struct OfferedProvider {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub models: Vec<String>,
    #[serde(rename = "allowedUsers")] pub allowed_users: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub max5h: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub max7d: Option<f64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Hello {
    pub nonce: String,
    pub user: String,
    pub providers: Vec<OfferedProvider>,
    pub engines: Vec<String>,
    pub pc: bool,
    pub version: String,
    pub os: String,
    pub caps: Vec<String>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Release { #[serde(default)] pub version: String, #[serde(default)] pub commit: String }
#[derive(Clone, Debug, Deserialize)]
pub struct HelloAnswer {
    #[serde(default)] pub id: String,
    #[serde(default)] pub nickname: String,
    #[serde(default)] pub user: String,
    #[serde(rename = "serverNonce", default)] pub server_nonce: String,
    #[serde(default)] pub kid: String,
    #[serde(rename = "pollMs", default)] pub poll_ms: Option<u64>,
    #[serde(default)] pub group: Vec<String>,
    #[serde(default)] pub update: Option<Release>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Beat { #[serde(default)] pub t: Option<u64>, #[serde(default)] pub version: String, #[serde(default)] pub commit: String }
/// A job from the hub: an engine / provider / login relay call, a PC tool, or a Work run (sealed in `enc`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Job {
    pub id: String,
    #[serde(default)] pub kind: String,
    #[serde(default)] pub engine: String,
    #[serde(default)] pub path: String,
    #[serde(default)] pub method: String,
    #[serde(default)] pub headers: Map<String, Value>,
    /// the request body, base64
    #[serde(default)] pub body: String,
    #[serde(default)] pub enc: Option<Envelope>,
}
impl Job {
    pub fn body_bytes(&self) -> Result<Vec<u8>> { if self.body.is_empty() { Ok(vec![]) } else { crate::unb64(&self.body) } }
    pub fn header(&self, k: &str) -> Option<&str> { self.headers.iter().find(|(h, _)| h.eq_ignore_ascii_case(k)).and_then(|(_, v)| v.as_str()) }
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PollAnswer { #[serde(default)] pub jobs: Vec<Job>, #[serde(default)] pub cancel: Vec<String>, #[serde(default)] pub beat: Option<Beat> }
pub enum Poll { Jobs(PollAnswer), Rehello, Unknown }
/// What /node/events answered: carry on, the person stopped the run, or the hub gave it up (stop it here too).
#[derive(Debug, PartialEq)]
pub enum EventsAnswer { Ok, Cancel, Gone }

/// One file in an update manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ManifestFile { pub name: String, pub sha256: String, #[serde(default, skip_serializing_if = "Option::is_none")] pub size: Option<u64> }

/// The connection to the hub: the client, this host's identity, and the session key once hello went through.
pub struct HubLink {
    pub hub: String,
    pub id: String,
    token: String,
    pub version: String,
    pub http: reqwest::Client,
    pub key: Option<HostKey>,
    pub poll_ms: u64,
}
impl HubLink {
    pub fn new(hub: &str, id: &str, token: &str, version: &str) -> Result<Self> {
        Ok(HubLink { hub: hub.trim_end_matches('/').to_string(), id: id.into(), token: token.into(), version: version.into(), http: crate::hub::http_client(Duration::from_secs(20))?, key: None, poll_ms: 25000 })
    }
    pub fn token(&self) -> &str { &self.token }
    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("X-VR-Host", &self.id).header("X-VR-Host-Token", &self.token).header("X-VR-Node", &self.id).header("X-VR-Node-Token", &self.token).header("X-VR-Host-Version", &self.version)
    }
    pub fn get(&self, path: &str) -> reqwest::RequestBuilder { self.auth(self.http.get(format!("{}{}", self.hub, path))) }
    pub fn post(&self, path: &str) -> reqwest::RequestBuilder { self.auth(self.http.post(format!("{}{}", self.hub, path))) }

    /// Says hello; on success the session key is set and checked against the hub's key id.
    pub async fn hello(&mut self, hello: &Hello) -> Result<HelloAnswer> {
        let r = self.post("/node/hello").json(hello).timeout(Duration::from_secs(20)).send().await?;
        if r.status().as_u16() == 401 { return Err(Error::status(401, "the hub does not know this host (id / token): check ai-node.json, or make the host again in Settings → My AI hosts")); }
        if !r.status().is_success() { return Err(Error::msg(format!("hello answered {}", r.status().as_u16()))); }
        let a: HelloAnswer = r.json().await?;
        let key = HostKey::derive(&self.token, &hello.nonce, &a.server_nonce)?;
        if key.id() != a.kid { return Err(Error::msg(format!("the key ids differ after the handshake (hub {}, here {}): is the token right?", a.kid, key.id()))); }
        self.key = Some(key);
        if let Some(ms) = a.poll_ms { self.poll_ms = ms; }
        Ok(a)
    }
    pub async fn poll(&self) -> Result<Poll> {
        let r = self.get("/node/poll").timeout(Duration::from_millis(self.poll_ms + 15000)).send().await?;
        match r.status().as_u16() {
            428 => Ok(Poll::Rehello),
            401 => Ok(Poll::Unknown),
            s if (200..300).contains(&s) => Ok(Poll::Jobs(r.json().await?)),
            s => Err(Error::msg(format!("hub answered {s}"))),
        }
    }
    /// A job's whole answer.
    pub async fn reply(&self, job_id: &str, status: u16, headers: &Map<String, Value>, body: &[u8]) {
        let b = json!({ "id": job_id, "status": status, "headers": headers, "body": if body.is_empty() { String::new() } else { b64(body) } });
        let _ = self.post("/node/reply").json(&b).timeout(Duration::from_secs(60)).send().await;
    }
    /// One piece of a streamed answer: false when the hub no longer waits for this job (stop reading it).
    pub async fn stream(&self, job_id: &str, part: Value) -> bool {
        let mut b = json!({ "id": job_id }); if let (Some(o), Value::Object(p)) = (b.as_object_mut(), part) { o.extend(p); }
        match self.post("/node/stream").json(&b).timeout(Duration::from_secs(60)).send().await { Ok(r) => r.status().as_u16() != 404, Err(_) => true }
    }
    /// A batch of a Work run's events, sealed with the session key.
    pub async fn events(&self, job_id: &str, batch: &Value) -> EventsAnswer {
        let Some(key) = &self.key else { return EventsAnswer::Ok };
        let Ok(enc) = key.seal(batch) else { return EventsAnswer::Ok };
        match self.post("/node/events").json(&json!({ "job": job_id, "enc": enc })).timeout(Duration::from_secs(30)).send().await {
            Ok(r) if r.status().as_u16() == 404 => EventsAnswer::Gone,
            Ok(r) => { let j: Value = r.json().await.unwrap_or(Value::Null); if j.get("cancel").and_then(Value::as_bool) == Some(true) { EventsAnswer::Cancel } else { EventsAnswer::Ok } }
            Err(_) => EventsAnswer::Ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jobs_parse_like_the_hub_sends_them() {
        let p: PollAnswer = serde_json::from_value(json!({ "jobs": [{ "id": "j1", "kind": "provider", "engine": "relay:claude", "path": "/v1/messages", "method": "POST", "headers": { "content-type": "application/json" }, "body": "e30=" }], "cancel": ["x"], "beat": { "t": 1, "version": "3.10.0", "commit": "abc" } })).unwrap();
        assert_eq!(p.jobs[0].body_bytes().unwrap(), b"{}");
        assert_eq!(p.jobs[0].header("Content-Type"), Some("application/json"));
        assert_eq!(p.beat.unwrap().version, "3.10.0");
        let empty: PollAnswer = serde_json::from_value(json!({})).unwrap();
        assert!(empty.jobs.is_empty() && empty.beat.is_none());
    }
}
