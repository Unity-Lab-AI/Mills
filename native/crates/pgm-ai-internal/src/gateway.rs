//! The API gateway's raw Anthropic passthrough (T-369, G2, plans/api-gateway.md section 3.4).
//!
//! `POST /v1/messages` to an **Anthropic-speaking provider** (an Anthropic API-key provider, or a host's Claude login
//! through its relay) is forwarded as it is: tools, tool_choice, thinking, images, documents, cache_control, a system
//! string or array, metadata, stop_sequences, temperature, everything. Only three things change: the `model` (the
//! resolved provider's real id when the caller used an alias), the auth (the provider's own key or the host's login,
//! never the caller's), and the headers (an allow-list: `anthropic-version`, `anthropic-beta`). The answer comes back raw
//! (status, SSE or JSON, `retry-after` and the harmless rate-limit headers); the usage is read on the way for the ledger
//! and the key's counters. The caller's slot (per-key concurrency) is held for the whole stream; the caller hanging up
//! frees it and closes the upstream.
//!
//! A Claude **subscription** login serves Claude Code only: Anthropic answers other harnesses with a refusal, which is
//! mapped to a plain message ([`SUBSCRIPTION_MESSAGE`]). Nothing here makes a request pass as Claude Code (no system prompt
//! added, no user agent): that was declined, it risks the host owner's account.
//!
//! Providers that do not speak Anthropic (OpenAI-compatible endpoints, Ollama, `host` / `claude-cli`) keep the text path in
//! `openai.rs` for now; cross-format translation is G3.
use bytes::Bytes;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::hub::{AiHub, Resp};
use crate::openai::{self, Running};
use pgm_core_internal::json::s;
use pgm_net_internal::http::Req;

pub const DEFAULT_VERSION: &str = "2023-06-01";
pub const DEFAULT_BASE: &str = "https://api.anthropic.com";
pub const SUBSCRIPTION_MESSAGE: &str = "This provider is a Claude subscription login: it serves Claude Code; use Claude Code with this key, or pick a provider that has an API key.";

/// Does this provider speak Anthropic's Messages API (so a request goes through raw)?
pub fn speaks_anthropic(p: &Value) -> bool {
    match p["kind"].as_str().unwrap_or("") { "anthropic" => true, "host-claude" => p["remoteKind"] != json!("claude-cli-work"), _ => false }
}
/// A host's Claude login (a subscription), as opposed to an API key.
pub fn is_login(p: &Value) -> bool { p["kind"] == json!("host-claude") }

/// The request headers that go upstream: `anthropic-version` (the caller's, else the default), every `anthropic-beta`
/// joined, `content-type`, and `accept` for a stream. Never the caller's auth, cookies or anything else.
pub fn request_headers(h: &hyper::HeaderMap, stream: bool) -> Vec<(String, String)> {
    let clean = |v: &hyper::header::HeaderValue| v.to_str().ok().map(|x| x.trim().to_string()).filter(|x| !x.is_empty() && x.len() <= 1000 && x.chars().all(|c| !c.is_control()));
    let version = h.get("anthropic-version").and_then(clean).unwrap_or_else(|| DEFAULT_VERSION.into());
    let betas: Vec<String> = h.get_all("anthropic-beta").iter().filter_map(clean).collect();
    let mut out = vec![("content-type".to_string(), "application/json".to_string()), ("anthropic-version".into(), version)];
    if !betas.is_empty() { out.push(("anthropic-beta".into(), betas.join(","))); }
    if stream { out.push(("accept".into(), "text/event-stream".into())); }
    out
}
/// The request body as it came with only `model` swapped.
pub fn upstream_body(b: &Value, model: &str) -> Vec<u8> {
    let mut b = b.clone();
    if !model.is_empty() { b["model"] = json!(model); }
    serde_json::to_vec(&b).unwrap_or_default()
}
/// The upstream answer's headers that go back to the caller: `request-id`, `retry-after` and the plain rate-limit numbers
/// (`anthropic-ratelimit-requests-*`, `-tokens-*`). Not the host's subscription windows (`anthropic-ratelimit-unified-*`:
/// the owner's business) and nothing else.
pub fn response_headers(h: &[(String, String)]) -> Vec<(String, String)> {
    h.iter().filter(|(k, _)| { let k = k.to_ascii_lowercase(); k == "request-id" || k == "retry-after" || (k.starts_with("anthropic-ratelimit-") && !k.starts_with("anthropic-ratelimit-unified-")) }).map(|(k, v)| (k.to_ascii_lowercase(), v.clone())).collect()
}
/// Is this the refusal Anthropic gives a third-party harness on a Claude subscription login? Recognised by its wording
/// ("... only authorized for use with Claude Code ...") or by the bare rate-limit error Anthropic sends with the message
/// "Error" and no usage window rejected (a real usage limit carries the window headers with status "rejected"). Only
/// asked for a login provider; an API key never gets this.
pub fn is_subscription_refusal(status: u16, headers: &[(String, String)], body: &[u8]) -> bool {
    if ![400, 401, 403, 429].contains(&status) { return false; }
    if headers.iter().any(|(k, v)| k.to_ascii_lowercase().starts_with("anthropic-ratelimit-unified-") && k.to_ascii_lowercase().ends_with("-status") && v.to_ascii_lowercase().contains("reject")) { return false; }
    let j: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let msg = j["error"]["message"].as_str().map(String::from).unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(600).collect()).to_lowercase();
    if msg.contains("claude code") && ["only authorized", "only be used", "cannot be used", "only available", "only supports", "only for", "not authorized", "only works"].iter().any(|w| msg.contains(w)) { return true; }
    status == 429 && j["error"]["type"] == json!("rate_limit_error") && msg.trim().trim_end_matches('.') == "error"
}
/// How many characters the tools of a request add to a rough token count (names, descriptions, schemas).
pub fn tools_chars(b: &Value) -> usize { b["tools"].as_array().map(|a| a.iter().map(|t| t.to_string().encode_utf16().count()).sum()).unwrap_or(0) }

/// The usage and model of an answer, read as it goes by: from an SSE stream (`message_start` carries the input and cache
/// numbers, `message_delta` the output) or from a whole JSON message.
#[derive(Default)]
pub struct UsageReader { buf: Vec<u8>, pub model: String, pub native: Value, pub stop_reason: String }
impl UsageReader {
    fn take(&mut self, u: &Value) {
        let Some(o) = u.as_object() else { return };
        if !self.native.is_object() { self.native = json!({}); }
        for (k, v) in o { if v.is_number() { self.native[k] = v.clone(); } }
    }
    fn event(&mut self, ev: &Value) {
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => { if let Some(m) = ev["message"]["model"].as_str() { self.model = m.into(); } self.take(&ev["message"]["usage"]); }
            "message_delta" => { self.take(&ev["usage"]); if let Some(r) = ev["delta"]["stop_reason"].as_str() { self.stop_reason = r.into(); } }
            _ => {}
        }
    }
    /// Feeds bytes of the stream (any split, a line may arrive in pieces).
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        while let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line); let line = line.trim();
            if let Some(d) = line.strip_prefix("data:") { if let Ok(v) = serde_json::from_str::<Value>(d.trim()) { self.event(&v); } }
        }
        if self.buf.len() > 4_000_000 { self.buf.clear(); } // a line this long is no event
    }
    /// A whole (non-streamed) message.
    pub fn from_json(body: &[u8]) -> UsageReader {
        let mut r = UsageReader::default(); let j: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        if let Some(m) = j["model"].as_str() { r.model = m.into(); } r.take(&j["usage"]);
        if let Some(x) = j["stop_reason"].as_str() { r.stop_reason = x.into(); }
        r
    }
}

/// An upstream body as it comes: from the internet or from an AI host.
enum Up { Http(reqwest::Response), Host(mpsc::Receiver<Result<Bytes, String>>) }
impl Up {
    async fn next(&mut self) -> Option<Result<Bytes, String>> {
        match self { Up::Http(r) => match r.chunk().await { Ok(Some(b)) => Some(Ok(b)), Ok(None) => None, Err(e) => Some(Err(e.to_string())) }, Up::Host(rx) => rx.recv().await }
    }
}

fn base_url(hub: &dyn AiHub) -> String {
    hub.cfg().get("api").and_then(|a| a["anthropicBase"].as_str()).map(|b| b.trim().trim_end_matches('/').to_string()).filter(|b| b.starts_with("http")).unwrap_or_else(|| DEFAULT_BASE.into())
}
fn failure(status: u16, m: &str) -> Resp {
    let status = if status == 0 { 502 } else { status };
    let t = match status { 503 => "overloaded_error", 429 => "rate_limit_error", 401 => "authentication_error", 400 => "invalid_request_error", 403 => "permission_error", _ => "api_error" };
    openai::cors(pgm_net_internal::http::json(status, &json!({ "type": "error", "error": { "type": t, "message": m } })))
}
fn raw(status: u16, ct: &str, hs: &[(String, String)], body: pgm_net_internal::http::Body) -> Resp {
    let mut b = hyper::Response::builder().status(status).header("content-type", if ct.is_empty() { "application/json" } else { ct });
    for (k, v) in hs { b = b.header(k.as_str(), v.as_str()); }
    openai::cors(b.header("cache-control", "no-store").header("x-accel-buffering", "no").body(body).expect("response"))
}
fn ct_of(h: &[(String, String)]) -> String { h.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|x| x.1.clone()).unwrap_or_default() }
/// Reads a whole body, giving up when the caller is gone.
async fn read_all(mut up: Up, cancel: &openai::Cancel) -> Option<Vec<u8>> {
    let mut v = vec![];
    loop { tokio::select! { _ = cancel.woke() => return None, c = up.next() => match c { Some(Ok(b)) => v.extend_from_slice(&b), Some(Err(_)) | None => return Some(v) } } }
}
fn note_usage(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, kind: &str, ur: &UsageReader) {
    if let Some(u) = openai::normalise(&ur.native) {
        hub.ai().usage.note_answer(user, kind, &ur.model, &u);
        openai::count_tokens(&**hub, user, key, u.total_input(), u.output);
    }
}
fn sse_error(m: &str) -> Bytes { Bytes::from(format!("event: error\ndata: {}\n\n", json!({ "type": "error", "error": { "type": "api_error", "message": m } }))) }

/// One Messages request, forwarded raw to `prov` (an Anthropic-speaking provider) as the person `user` on `key`. `model` is
/// the real model id the routing resolved; `b` the request's parsed JSON.
pub async fn messages(hub: &Arc<dyn AiHub>, r: &Req, user: &str, key: &openai::KeyCtx, prov: &Value, model: &str, b: Value) -> Resp {
    let (w, ai) = (hub.work(), hub.ai());
    let id = s(prov, "id");
    let p = w.provider_get(ai, &id, user).unwrap_or_else(|| prov.clone()); // the full record: the provider's own key is in it
    let kind = s(&p, "kind");
    if kind == "host-claude" && hub.root().join("ai-host.off").exists() { return failure(503, "The host's AI is switched off right now (its host paused it); an API key or endpoint of your own still answers."); }
    if kind == "anthropic" && s(&p, "apiKey").is_empty() { return failure(503, "This provider has no API key set."); }
    let Some(run) = openai::start_running(hub, user, key) else { return openai::too_many(true, key, openai::max_concurrent(&**hub, key)) };
    openai::count_use(&**hub, user, key);
    let stream = b["stream"].as_bool().unwrap_or(false);
    let headers = request_headers(&r.headers, stream);
    let body = upstream_body(&b, model);
    let cancel = run.cancel.clone();
    let (host, pid) = (s(&p, "hostId"), s(&p, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default());
    let (status, rheaders, up) = if kind == "anthropic" {
        let mut rb = openai::self_http(&**hub).post(format!("{}/v1/messages", base_url(&**hub))).header("x-api-key", s(&p, "apiKey")).body(body);
        for (k, v) in &headers { rb = rb.header(k.as_str(), v.as_str()); }
        let sent = tokio::select! { _ = cancel.woke() => return failure(499, "cancelled"), x = rb.send() => x };
        let resp = match sent { Ok(x) => x, Err(e) => return failure(502, &format!("Anthropic could not be reached: {e}")) };
        let hs: Vec<(String, String)> = resp.headers().iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
        (resp.status().as_u16(), hs, Up::Http(resp))
    } else {
        let sent = tokio::select! { _ = cancel.woke() => return failure(499, "cancelled"), x = ai.login_fetch_for(user, "api", &host, &pid, "/v1/messages", "POST", headers, body) => x };
        match sent { Ok(x) => (x.status, x.headers, Up::Host(x.body)), Err(e) => return failure(e.status, &e.msg) }
    };
    let ct = ct_of(&rheaders); let out_h = response_headers(&rheaders);
    if !(200..300).contains(&status) {
        let Some(bytes) = read_all(up, &cancel).await else { return failure(499, "cancelled") };
        if is_login(&p) && is_subscription_refusal(status, &rheaders, &bytes) {
            return failure(403, SUBSCRIPTION_MESSAGE);
        }
        return raw(status, &ct, &out_h, pgm_net_internal::http::full(bytes)); // Anthropic's own error, as it came
    }
    if !stream {
        let Some(bytes) = read_all(up, &cancel).await else { return failure(499, "cancelled") };
        note_usage(hub, user, key, &kind, &UsageReader::from_json(&bytes));
        drop(run);
        return raw(status, &ct, &out_h, pgm_net_internal::http::full(bytes));
    }
    // streamed: bytes go on as they come; the slot is held by the task until the stream ends or the caller hangs up
    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, String>>();
    let h = hub.clone(); let user = user.to_string(); let key = key.clone();
    tokio::spawn(forward(h, user, key, kind, up, tx, run, cancel));
    use http_body_util::BodyExt;
    let st = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x.map(hyper::body::Frame::data).map_err(Box::<dyn std::error::Error + Send + Sync>::from), rx)) });
    raw(status, if ct.is_empty() { "text/event-stream" } else { &ct }, &out_h, http_body_util::StreamBody::new(st).boxed())
}
/// The stream's pump: upstream chunks to the caller untouched, the usage read on the way. Ends when the upstream does, when
/// the caller hangs up (the channel closes: the upstream is dropped, which closes it) or when the answer is cancelled.
#[allow(clippy::too_many_arguments)]
async fn forward(hub: Arc<dyn AiHub>, user: String, key: openai::KeyCtx, kind: String, mut up: Up, tx: mpsc::UnboundedSender<Result<Bytes, String>>, run: Running, cancel: Arc<openai::Cancel>) {
    let mut ur = UsageReader::default();
    loop {
        tokio::select! {
            _ = tx.closed() => break,
            _ = cancel.woke() => { let _ = tx.send(Ok(sse_error("cancelled"))); break; }
            c = up.next() => match c {
                Some(Ok(bytes)) => { ur.push(&bytes); if tx.send(Ok(bytes)).is_err() { break; } }
                Some(Err(e)) => { let _ = tx.send(Ok(sse_error(&format!("the upstream stream broke: {e}")))); break; }
                None => break,
            },
        }
    }
    drop(up); // closes the upstream
    note_usage(&hub, &user, &key, &kind, &ur);
    drop(run); // frees the key's slot
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hm(v: &[(&str, &str)]) -> hyper::HeaderMap { let mut h = hyper::HeaderMap::new(); for (k, x) in v { h.append(hyper::header::HeaderName::from_bytes(k.as_bytes()).unwrap(), x.parse().unwrap()); } h }

    #[test]
    fn only_the_allow_listed_request_headers_go_upstream() {
        let h = hm(&[("x-api-key", "mills_secret"), ("authorization", "Bearer mills_secret"), ("cookie", "a=b"), ("anthropic-version", "2024-01-01"), ("anthropic-beta", "a-1,b-2"), ("anthropic-beta", "c-3"), ("user-agent", "harness/1"), ("x-app", "cli")]);
        let out = request_headers(&h, true);
        let names: Vec<&str> = out.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(names, ["content-type", "anthropic-version", "anthropic-beta", "accept"]);
        assert_eq!(out[1].1, "2024-01-01"); assert_eq!(out[2].1, "a-1,b-2,c-3");
        let none = request_headers(&hm(&[]), false);
        assert_eq!(none, vec![("content-type".to_string(), "application/json".to_string()), ("anthropic-version".into(), DEFAULT_VERSION.into())]);
    }
    #[test]
    fn only_the_model_changes_in_the_body() {
        let b = json!({ "model": "gee/claude-opus", "max_tokens": 9, "system": [{ "type": "text", "text": "s", "cache_control": { "type": "ephemeral" } }], "tools": [{ "name": "t", "input_schema": { "type": "object" } }], "tool_choice": { "type": "any" }, "thinking": { "type": "enabled", "budget_tokens": 1024 }, "metadata": { "user_id": "u" }, "stop_sequences": ["x"], "temperature": 0.5, "messages": [{ "role": "user", "content": [{ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }] }] });
        let out: Value = serde_json::from_slice(&upstream_body(&b, "claude-opus")).unwrap();
        let mut want = b.clone(); want["model"] = json!("claude-opus");
        assert_eq!(out, want);
    }
    #[test]
    fn response_headers_keep_the_harmless_ones() {
        let h = vec![("Retry-After".to_string(), "3".to_string()), ("anthropic-ratelimit-requests-remaining".into(), "9".into()), ("anthropic-ratelimit-unified-5h-utilization".into(), "0.4".into()), ("set-cookie".into(), "x".into()), ("request-id".into(), "req_1".into())];
        let out: Vec<String> = response_headers(&h).into_iter().map(|x| x.0).collect();
        assert_eq!(out, ["retry-after", "anthropic-ratelimit-requests-remaining", "request-id"]);
    }
    #[test]
    fn the_subscription_refusal_is_recognised_by_wording_and_never_for_a_real_usage_limit() {
        let msg = br#"{"type":"error","error":{"type":"invalid_request_error","message":"This credential is only authorized for use with Claude Code and cannot be used for other API requests."}}"#;
        assert!(is_subscription_refusal(429, &[], msg)); assert!(is_subscription_refusal(400, &[], msg));
        let bare = br#"{"type":"error","error":{"type":"rate_limit_error","message":"Error"}}"#;
        assert!(is_subscription_refusal(429, &[], bare));
        let rejected = vec![("anthropic-ratelimit-unified-5h-status".to_string(), "rejected".to_string())];
        assert!(!is_subscription_refusal(429, &rejected, bare));
        assert!(!is_subscription_refusal(429, &[], br#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}"#));
        assert!(!is_subscription_refusal(500, &[], msg)); assert!(!is_subscription_refusal(200, &[], msg));
        assert!(!is_subscription_refusal(401, &[], br#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#));
    }
    #[test]
    fn usage_is_read_from_a_stream_split_anywhere() {
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-x\",\"usage\":{\"input_tokens\":11,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":2,\"output_tokens\":1}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"é\\\"\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":42}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        for step in [1usize, 7, 1000] {
            let mut r = UsageReader::default();
            for c in sse.as_bytes().chunks(step) { r.push(c); }
            assert_eq!(r.model, "claude-x"); assert_eq!(r.stop_reason, "tool_use");
            let u = openai::normalise(&r.native).unwrap();
            assert_eq!((u.input, u.output, u.cache_read, u.cache_write), (11, 42, 5, 2), "step {step}");
        }
    }
    #[test]
    fn usage_is_read_from_a_whole_message() {
        let r = UsageReader::from_json(br#"{"id":"m","model":"claude-y","stop_reason":"end_turn","content":[],"usage":{"input_tokens":3,"output_tokens":4,"service_tier":"standard"}}"#);
        assert_eq!(r.model, "claude-y"); let u = openai::normalise(&r.native).unwrap(); assert_eq!((u.input, u.output), (3, 4));
        assert!(openai::normalise(&UsageReader::from_json(b"not json").native).is_none());
    }
    #[test]
    fn which_providers_speak_anthropic() {
        assert!(speaks_anthropic(&json!({ "kind": "anthropic" })));
        assert!(speaks_anthropic(&json!({ "kind": "host-claude", "remoteKind": "claude-relay" })));
        assert!(!speaks_anthropic(&json!({ "kind": "host-claude", "remoteKind": "claude-cli-work" })));
        for k in ["openai", "host-openai", "host", "claude-cli", "host-codex", "host-jcode"] { assert!(!speaks_anthropic(&json!({ "kind": k })), "{k}"); }
    }
    #[test]
    fn tools_add_to_the_rough_count() {
        assert_eq!(tools_chars(&json!({})), 0);
        assert!(tools_chars(&json!({ "tools": [{ "name": "a", "input_schema": {} }] })) > 10);
    }
}
