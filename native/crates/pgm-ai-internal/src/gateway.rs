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
//! G3 (the same slice of the plan, section 3.4): `POST /v1/chat/completions` to an **OpenAI-speaking provider** (an
//! OpenAI-compatible endpoint, a host's `host-openai` / Ollama) is forwarded the same way ([`chat_raw`]): the body as
//! parsed with only `model` swapped, the provider's own auth, the SSE or JSON answer and every error as they came, the usage
//! read from the stream (`stream_options.include_usage` is asked for unless the caller forbade it). **Cross-format**: an
//! OpenAI request to an Anthropic-speaking provider ([`chat_via_anthropic`]) and an Anthropic request to an OpenAI-speaking
//! one ([`messages_via_openai`]) are translated by [`crate::xlate`] (tools, images, system, streaming both ways) and the
//! answer comes back in the shape the caller speaks; what cannot be carried is refused with a 400 naming it. All four
//! flows share one stream pump ([`Pump`]): the slot is held to the end, the caller hanging up closes the upstream.
//!
//! G5: `POST /v1/responses` (OpenAI's Responses API, Codex) on the same pump ([`responses`]): raw to a provider that speaks it
//! (an endpoint at api.openai.com or marked `responses: true`, a host's ChatGPT / Codex login), translated to chat completions
//! or Messages for the others. Every flow's usage also goes to the key's day roll-up (`openai::count_use` /
//! `openai::count_tokens`, under the provider and model the call was routed to).
//!
//! Providers that speak neither (`host` / `claude-cli`, Codex, Jcode) keep the text path in `openai.rs`; so does a plain text
//! chat request to a Claude login (`host-claude`), which that path has always sent as Claude Code (an older arrangement;
//! requests with tools or images take the translator and are sent as they are).
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
/// Does this provider speak OpenAI's chat completions (so a chat request goes through raw)? An endpoint of the admin's, or an
/// OpenAI-compatible server (Ollama, vLLM, LM Studio ...) a host lends.
pub fn speaks_openai(p: &Value) -> bool { matches!(p["kind"].as_str().unwrap_or(""), "openai" | "openrouter" | "host-openai") }
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
enum Up { Http(reqwest::Response), Host(mpsc::Receiver<Result<Bytes, String>>), Mem(Option<Bytes>), Pre(Option<Bytes>, reqwest::Response) }
impl Up {
    async fn next(&mut self) -> Option<Result<Bytes, String>> {
        match self { Up::Http(r) => match r.chunk().await { Ok(Some(b)) => Some(Ok(b)), Ok(None) => None, Err(e) => Some(Err(e.to_string())) }, Up::Host(rx) => rx.recv().await, Up::Mem(b) => b.take().map(Ok), Up::Pre(h, r) => match h.take() { Some(b) => Some(Ok(b)), None => match r.chunk().await { Ok(Some(b)) => Some(Ok(b)), Ok(None) => None, Err(e) => Some(Err(e.to_string())) } } }
    }
}
type Upstream = (u16, Vec<(String, String)>, Up);
/// Which shape the caller speaks, so an error comes back in it.
#[derive(Clone, Copy, PartialEq)]
enum Fmt { Ant, Oa }
fn err_resp(f: Fmt, status: u16, m: &str) -> Resp {
    match f {
        Fmt::Ant => failure(status, m),
        Fmt::Oa => {
            let status = if status == 0 { 502 } else { status };
            let t = match status { 503 => "service_unavailable", 429 => "rate_limit_error", 401 => "authentication_error", 400 => "invalid_request_error", 403 => "permission_error", _ => "api_error" };
            openai::oa_error(status, m, t)
        }
    }
}
/// `retry-after` of an upstream answer, put on an error we made from it.
fn with_retry(mut r: Resp, hs: &[(String, String)]) -> Resp {
    if let Some(v) = hs.iter().find(|(k, _)| k.eq_ignore_ascii_case("retry-after")).and_then(|(_, v)| hyper::header::HeaderValue::from_str(v).ok()) { r.headers_mut().insert("retry-after", v); }
    with_switch(r, hs)
}
/// An OpenRouter entry's chain switches (x-vr-openrouter-switch, one per switch) on an answer the hub built itself (a
/// translated whole answer, an error after every step failed), as the raw paths pass them on (T-381).
fn with_switch(mut r: Resp, hs: &[(String, String)]) -> Resp {
    for (_, v) in hs.iter().filter(|(k, _)| k.eq_ignore_ascii_case("x-vr-openrouter-switch")) { if let Ok(v) = hyper::header::HeaderValue::from_str(v) { r.headers_mut().append("x-vr-openrouter-switch", v); } }
    r
}
/// The words of an upstream error body (Anthropic's and OpenAI's both: `error.message`), else the start of the body.
fn upstream_message(status: u16, body: &[u8]) -> String {
    let j: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let m = j["error"]["message"].as_str().or(j["error"].as_str()).or(j["message"].as_str()).map(String::from).unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(300).collect());
    if m.trim().is_empty() { format!("The provider answered {status}.") } else { m }
}
/// The upstream answer's headers of an OpenAI-speaking provider that go back: `retry-after`, the request id and the plain
/// rate-limit numbers.
pub fn oa_response_headers(h: &[(String, String)]) -> Vec<(String, String)> {
    h.iter().filter(|(k, _)| { let k = k.to_ascii_lowercase(); k == "retry-after" || k == "x-request-id" || k.starts_with("x-ratelimit-") || k == "x-vr-openrouter-switch" }).map(|(k, v)| (k.to_ascii_lowercase(), v.clone())).collect()
}

/// What a stream's pump needs of a flow: the bytes to send for the bytes that came (raw: the same; translated: the other
/// shape's events), the end, an error in the caller's shape, and the usage read on the way.
trait Pump: Send {
    fn push(&mut self, c: &[u8]) -> Vec<Bytes>;
    fn finish(&mut self) -> Vec<Bytes> { vec![] }
    fn error(&mut self, m: &str) -> Vec<Bytes>;
    /// (the model the upstream named, its usage object as the provider gave it)
    fn usage(&self) -> (String, Value);
}
fn frames(v: Vec<String>) -> Vec<Bytes> { v.into_iter().map(Bytes::from).collect() }
struct RawAnt(UsageReader);
impl Pump for RawAnt {
    fn push(&mut self, c: &[u8]) -> Vec<Bytes> { self.0.push(c); vec![Bytes::copy_from_slice(c)] }
    fn error(&mut self, m: &str) -> Vec<Bytes> { vec![sse_error(m)] }
    fn usage(&self) -> (String, Value) { (self.0.model.clone(), self.0.native.clone()) }
}
/// The model and usage of an OpenAI-shaped answer, read as it goes by: from the stream's chunks (the usage comes in the last
/// one, when asked for) or from a whole `chat.completion`.
#[derive(Default)]
pub struct OaUsage { sse: crate::xlate::SseData, pub model: String, pub native: Value }
impl OaUsage {
    fn take(&mut self, j: &Value) {
        if let Some(m) = j["model"].as_str().filter(|m| !m.is_empty()) { self.model = m.into(); }
        if j["usage"].is_object() { self.native = j["usage"].clone(); }
    }
    pub fn push(&mut self, c: &[u8]) { for d in self.sse.push(c) { if let Ok(j) = serde_json::from_str::<Value>(&d) { self.take(&j); } } }
    pub fn from_json(body: &[u8]) -> OaUsage { let mut r = OaUsage::default(); r.take(&serde_json::from_slice::<Value>(body).unwrap_or(Value::Null)); r }
}
struct RawOa(OaUsage);
impl Pump for RawOa {
    fn push(&mut self, c: &[u8]) -> Vec<Bytes> { self.0.push(c); vec![Bytes::copy_from_slice(c)] }
    fn error(&mut self, m: &str) -> Vec<Bytes> { vec![Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", json!({ "error": { "message": m, "type": "api_error", "code": Value::Null } })))] }
    fn usage(&self) -> (String, Value) { (self.0.model.clone(), self.0.native.clone()) }
}
struct AntToOaPump(crate::xlate::AntToOa);
impl Pump for AntToOaPump {
    fn push(&mut self, c: &[u8]) -> Vec<Bytes> { frames(self.0.push(c)) }
    fn finish(&mut self) -> Vec<Bytes> { frames(self.0.finish()) }
    fn error(&mut self, m: &str) -> Vec<Bytes> { frames(self.0.error(m)) }
    fn usage(&self) -> (String, Value) { (self.0.model().to_string(), self.0.native().clone()) }
}
struct OaToAntPump(crate::xlate::OaToAnt);
impl Pump for OaToAntPump {
    fn push(&mut self, c: &[u8]) -> Vec<Bytes> { frames(self.0.push(c)) }
    fn finish(&mut self) -> Vec<Bytes> { frames(self.0.finish()) }
    fn error(&mut self, m: &str) -> Vec<Bytes> { frames(self.0.error(m)) }
    fn usage(&self) -> (String, Value) { (self.0.model().to_string(), self.0.native().clone()) }
}
/// A native Responses stream, byte for byte; its usage read from `response.completed`. An upstream that breaks off gets an
/// `error` event numbered after the last one that came.
struct RawResp(crate::xlate::RespReader);
impl Pump for RawResp {
    fn push(&mut self, c: &[u8]) -> Vec<Bytes> { self.0.push(c); vec![Bytes::copy_from_slice(c)] }
    fn error(&mut self, m: &str) -> Vec<Bytes> {
        if self.0.done.is_some() { return vec![]; }
        let n = self.0.last_seq.map(|n| n + 1).unwrap_or(0);
        vec![Bytes::from(format!("event: error\ndata: {}\n\n", json!({ "type": "error", "sequence_number": n, "code": "server_error", "message": m, "param": Value::Null })))]
    }
    fn usage(&self) -> (String, Value) { (self.0.model.clone(), self.0.native.clone()) }
}
struct ChatToRespPump(crate::xlate::ChatToResp);
impl Pump for ChatToRespPump {
    fn push(&mut self, c: &[u8]) -> Vec<Bytes> { frames(self.0.push(c)) }
    fn finish(&mut self) -> Vec<Bytes> { frames(self.0.finish()) }
    fn error(&mut self, m: &str) -> Vec<Bytes> { frames(self.0.error(m)) }
    fn usage(&self) -> (String, Value) { (self.0.model().to_string(), self.0.native().clone()) }
}
struct AntToRespPump(crate::xlate::AntToResp);
impl Pump for AntToRespPump {
    fn push(&mut self, c: &[u8]) -> Vec<Bytes> { frames(self.0.push(c)) }
    fn finish(&mut self) -> Vec<Bytes> { frames(self.0.finish()) }
    fn error(&mut self, m: &str) -> Vec<Bytes> { frames(self.0.error(m)) }
    fn usage(&self) -> (String, Value) { (self.0.model().to_string(), self.0.native().clone()) }
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
/// Where an answer's usage is counted: the provider's kind (the ledger), its id and the model it was routed to (the key's day
/// roll-up, G5).
#[derive(Clone)]
struct Tally { kind: String, prov: String, model: String, personal: bool }
fn tally(p: &Value, model: &str) -> Tally { Tally { kind: s(p, "kind"), prov: s(p, "id"), model: model.to_string(), personal: crate::openrouter::is_personal(p) } }
/// A call starts on its key: counted, in the roll-up under its provider and model.
fn count_call(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, t: &Tally) { openai::count_use(&**hub, user, key, &t.prov, &t.model); }
fn note_usage(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, t: &Tally, ur: &UsageReader) { note_native(hub, user, key, t, &ur.model, &ur.native); }
/// An answer's usage (the provider's own object, whichever shape) to the ledger (under the model the upstream named) and the
/// key's counters and day roll-up (under the routed model).
fn note_native(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, t: &Tally, model: &str, native: &Value) {
    if t.kind == crate::openrouter::KIND { crate::openrouter::note_for_in(&**hub, user, t.personal, native, &key.session); } // the month budget and the team key's limits (T-381): OpenRouter's own cost of the answer
    if let Some(u) = openai::normalise(native) {
        hub.ai().usage.note_answer(user, &t.kind, model, &u);
        openai::count_tokens(&**hub, user, key, &t.prov, &t.model, u.total_input(), u.output);
    }
}
/// An answer another program made for the hub (the Jcode PGM Server, T-376) counted exactly as one the gateway made on
/// `p`: the call on the key, then its usage (`native`: the provider's own usage object) into the month budget or the
/// person's own-key counter, the usage ledger (under `model`, the upstream's id) and the key's tokens (under `routed`).
pub fn charge_external(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, p: &Value, routed: &str, model: &str, native: &Value) {
    let t = tally(p, routed);
    count_call(hub, user, key, &t);
    note_native(hub, user, key, &t, model, native);
}
/// The provider's full record (its own key is in it), else what routing gave.
fn full_provider(hub: &Arc<dyn AiHub>, user: &str, prov: &Value) -> Value { hub.work().provider_get(hub.ai(), &s(prov, "id"), user).unwrap_or_else(|| prov.clone()) }
/// Is this provider unable to answer right now (its host paused its AI, it has no key)? The refusal, in the caller's shape.
fn preflight(hub: &Arc<dyn AiHub>, user: &str, p: &Value, f: Fmt) -> Option<Resp> {
    let kind = s(p, "kind");
    // OpenRouter costs money: a person at or over their month's cap is refused before OpenRouter is asked (openrouter.rs)
    if kind == crate::openrouter::KIND { if let Some(m) = crate::openrouter::guard(&**hub, user, p) { return Some(err_resp(f, 429, &m)); } }
    if kind == "host-claude" && hub.root().join("ai-host.off").exists() { return Some(err_resp(f, 503, "The host's AI is switched off right now (its host paused it); an API key or endpoint of your own still answers.")); }
    if kind == "anthropic" && s(p, "apiKey").is_empty() { return Some(err_resp(f, 503, "This provider has no API key set.")); }
    if kind == "openai" && s(p, "baseUrl").is_empty() { return Some(err_resp(f, 503, "This provider has no address set.")); }
    if kind == crate::openrouter::KIND && s(p, "apiKey").is_empty() { return Some(err_resp(f, 503, "This OpenRouter provider has no key set.")); }
    None
}
/// The request to an Anthropic-speaking provider: Anthropic with the provider's key, or a host's Claude login through its relay.
async fn ant_upstream(hub: &Arc<dyn AiHub>, user: &str, p: &Value, headers: Vec<(String, String)>, body: Vec<u8>, cancel: &Arc<openai::Cancel>, f: Fmt) -> Result<Upstream, Resp> {
    let kind = s(p, "kind");
    let (host, pid) = (s(p, "hostId"), s(p, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default());
    if kind == "anthropic" {
        let mut rb = openai::self_http(&**hub).post(format!("{}/v1/messages", base_url(&**hub))).header("x-api-key", s(p, "apiKey")).body(body);
        for (k, v) in &headers { rb = rb.header(k.as_str(), v.as_str()); }
        let sent = tokio::select! { _ = cancel.woke() => return Err(err_resp(f, 499, "cancelled")), x = rb.send() => x };
        let resp = match sent { Ok(x) => x, Err(e) => return Err(err_resp(f, 502, &format!("Anthropic could not be reached: {e}"))) };
        let hs: Vec<(String, String)> = resp.headers().iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
        Ok((resp.status().as_u16(), hs, Up::Http(resp)))
    } else {
        let sent = tokio::select! { _ = cancel.woke() => return Err(err_resp(f, 499, "cancelled")), x = hub.ai().login_fetch_for(user, "api", &host, &pid, "/v1/messages", "POST", headers, body) => x };
        match sent { Ok(x) => Ok((x.status, x.headers, Up::Host(x.body))), Err(e) => Err(err_resp(f, e.status, &e.msg)) }
    }
}
/// The chat completions path of an OpenAI-speaking provider (under its base URL).
const CHAT: &str = "/chat/completions";
/// The request to an OpenAI-speaking provider at `path` (chat completions, or a native Responses provider's /responses): an
/// endpoint with its key as a bearer, or a host's server through its relay.
#[allow(clippy::too_many_arguments)]
async fn oa_upstream(hub: &Arc<dyn AiHub>, user: &str, p: &Value, path: &str, body: Vec<u8>, stream: bool, cancel: &Arc<openai::Cancel>, f: Fmt) -> Result<Upstream, Resp> {
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
    if stream { headers.push(("accept".into(), "text/event-stream".into())); }
    if s(p, "kind") == crate::openrouter::KIND {
        // OpenRouter (openrouter.rs): its fixed address, the entry's chain (each step's model on exactly its endpoint, the next
        // while one fails before it answers: a stream is peeked at for an error as its first event), usage with the cost, the
        // hub's referer and title; the switches go to the caller as x-vr-openrouter-switch, one line each (T-381)
        let (sent, lines) = tokio::select! { _ = cancel.woke() => return Err(err_resp(f, 499, "cancelled")), x = crate::openrouter::send_chain(&**hub, p, path, &body, &headers, stream) => x };
        if !lines.is_empty() { eprintln!("[vr-hub] openrouter for {user}: {}", lines.join(" | ")); }
        let c = match sent { Ok(c) => c, Err(e) => return Err(err_resp(f, 502, &format!("OpenRouter could not be reached: {e}"))) };
        let st = c.resp.status().as_u16();
        // a person's own key that OpenRouter refused: say so plainly, never retry with the team's
        if let Some(m) = crate::openrouter::refused(p, st) { return Err(err_resp(f, 502, &m)); }
        let mut hs: Vec<(String, String)> = c.resp.headers().iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
        for l in &lines { hs.push(("x-vr-openrouter-switch".into(), l.chars().map(|ch| if ch.is_ascii() && !ch.is_control() { ch } else { '?' }).collect())); }
        Ok((st, hs, if c.head.is_empty() { Up::Http(c.resp) } else { Up::Pre(Some(c.head), c.resp) }))
    } else if s(p, "kind") == "openai" {
        let mut rb = openai::self_http(&**hub).post(format!("{}{path}", s(p, "baseUrl").trim_end_matches('/'))).body(body);
        for (k, v) in &headers { rb = rb.header(k.as_str(), v.as_str()); }
        if !s(p, "apiKey").is_empty() { rb = rb.header("authorization", format!("Bearer {}", s(p, "apiKey"))); }
        let sent = tokio::select! { _ = cancel.woke() => return Err(err_resp(f, 499, "cancelled")), x = rb.send() => x };
        let resp = match sent { Ok(x) => x, Err(e) => return Err(err_resp(f, 502, &format!("The endpoint could not be reached: {e}"))) };
        let hs: Vec<(String, String)> = resp.headers().iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
        Ok((resp.status().as_u16(), hs, Up::Http(resp)))
    } else {
        let (host, pid) = (s(p, "hostId"), s(p, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default());
        let sent = tokio::select! { _ = cancel.woke() => return Err(err_resp(f, 499, "cancelled")), x = hub.ai().provider_fetch_for(user, "api", &host, &pid, path, "POST", headers, body) => x };
        match sent { Ok(x) => Ok((x.status, x.headers, Up::Host(x.body))), Err(e) => Err(err_resp(f, e.status, &e.msg)) }
    }
}
fn sse_error(m: &str) -> Bytes { Bytes::from(format!("event: error\ndata: {}\n\n", json!({ "type": "error", "error": { "type": "api_error", "message": m } }))) }

/// One Messages request, forwarded raw to `prov` (an Anthropic-speaking provider) as the person `user` on `key`. `model` is
/// the real model id the routing resolved; `b` the request's parsed JSON.
pub async fn messages(hub: &Arc<dyn AiHub>, r: &Req, user: &str, key: &openai::KeyCtx, prov: &Value, model: &str, b: Value) -> Resp {
    let p = full_provider(hub, user, prov); // the full record: the provider's own key is in it
    let t = tally(&p, model);
    if let Some(e) = preflight(hub, user, &p, Fmt::Ant) { return e; }
    let Some(run) = openai::start_running(hub, user, key) else { return openai::too_many(true, key, openai::max_concurrent(&**hub, key)) };
    count_call(hub, user, key, &t);
    let stream = b["stream"].as_bool().unwrap_or(false);
    let cancel = run.cancel.clone();
    let (status, rheaders, up) = match ant_upstream(hub, user, &p, request_headers(&r.headers, stream), upstream_body(&b, model), &cancel, Fmt::Ant).await { Ok(x) => x, Err(e) => return e };
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
        note_usage(hub, user, key, &t, &UsageReader::from_json(&bytes));
        drop(run);
        return raw(status, &ct, &out_h, pgm_net_internal::http::full(bytes));
    }
    // streamed: bytes go on as they come; the slot is held by the task until the stream ends or the caller hangs up
    streamed(hub, user, key, &t, up, Box::new(RawAnt(UsageReader::default())), run, status, if ct.is_empty() { "text/event-stream" } else { &ct }, &out_h)
}
/// Starts the pump of a stream and answers with its body.
#[allow(clippy::too_many_arguments)]
fn streamed(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, t: &Tally, up: Up, pump: Box<dyn Pump>, run: Running, status: u16, ct: &str, out_h: &[(String, String)]) -> Resp {
    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, String>>();
    let cancel = run.cancel.clone();
    tokio::spawn(forward(hub.clone(), user.to_string(), key.clone(), t.clone(), up, pump, tx, run, cancel));
    use http_body_util::BodyExt;
    let st = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x.map(hyper::body::Frame::data).map_err(Box::<dyn std::error::Error + Send + Sync>::from), rx)) });
    raw(status, ct, out_h, http_body_util::StreamBody::new(st).boxed())
}
/// The stream's pump: upstream chunks through the flow's [`Pump`] (untouched for a raw one, translated for a cross-format
/// one), the usage read on the way. Ends when the upstream does, when the caller hangs up (the channel closes: the upstream
/// is dropped, which closes it) or when the answer is cancelled.
#[allow(clippy::too_many_arguments)]
async fn forward(hub: Arc<dyn AiHub>, user: String, key: openai::KeyCtx, t: Tally, mut up: Up, mut pump: Box<dyn Pump>, tx: mpsc::UnboundedSender<Result<Bytes, String>>, run: Running, cancel: Arc<openai::Cancel>) {
    let send = |tx: &mpsc::UnboundedSender<Result<Bytes, String>>, v: Vec<Bytes>| v.into_iter().all(|b| tx.send(Ok(b)).is_ok());
    loop {
        tokio::select! {
            _ = tx.closed() => break,
            _ = cancel.woke() => { let _ = send(&tx, pump.error("cancelled")); break; }
            c = up.next() => match c {
                Some(Ok(bytes)) => { if !send(&tx, pump.push(&bytes)) { break; } }
                Some(Err(e)) => { let _ = send(&tx, pump.error(&format!("the upstream stream broke: {e}"))); break; }
                None => { let _ = send(&tx, pump.finish()); break; }
            },
        }
    }
    drop(up); // closes the upstream
    let (model, native) = pump.usage();
    note_native(&hub, &user, &key, &t, &model, &native);
    drop(run); // frees the key's slot
}

/// `POST /v1/chat/completions` to an OpenAI-speaking provider, raw (G3): the body as parsed with only `model` swapped, the
/// provider's own auth, the answer (SSE byte for byte, JSON as sent, errors with their status, `retry-after` and rate-limit
/// headers) as it came. A stream asks for `stream_options.include_usage` unless the caller said `false` (an endpoint that
/// refuses the option is asked again without it) and the usage goes to the ledger and the key's counters. An endpoint that
/// answers JSON to a streaming request is streamed from that JSON.
pub async fn chat_raw(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, prov: &Value, model: &str, b: Value) -> Resp {
    let p = full_provider(hub, user, prov);
    let t = tally(&p, model);
    if let Some(m) = crate::toolconv::check_tools(&b) { return err_resp(Fmt::Oa, 400, &m); } // a malformed tools field is refused here, as it always was
    if let Some(e) = preflight(hub, user, &p, Fmt::Oa) { return e; }
    let Some(run) = openai::start_running(hub, user, key) else { return openai::too_many(false, key, openai::max_concurrent(&**hub, key)) };
    count_call(hub, user, key, &t);
    let stream = b["stream"].as_bool().unwrap_or(false);
    let asked_usage = b["stream_options"]["include_usage"] == json!(true);
    let mut added = stream && b["stream_options"]["include_usage"] != json!(false);
    let mut body = b.clone();
    if !model.is_empty() { body["model"] = json!(model); }
    if added { if !body["stream_options"].is_object() { body["stream_options"] = json!({}); } body["stream_options"]["include_usage"] = json!(true); }
    let cancel = run.cancel.clone();
    // an OpenRouter entry's chain is walked inside oa_upstream (T-381), for every way in
    let (status, rheaders, up) = loop {
        let (st, hs, up) = match oa_upstream(hub, user, &p, CHAT, serde_json::to_vec(&body).unwrap_or_default(), stream, &cancel, Fmt::Oa).await { Ok(x) => x, Err(e) => return e };
        if added && (st == 400 || st == 422) {
            let Some(bytes) = read_all(up, &cancel).await else { return err_resp(Fmt::Oa, 499, "cancelled") };
            let t = String::from_utf8_lossy(&bytes).to_lowercase();
            if t.contains("stream_options") || t.contains("include_usage") { added = false; if let Some(o) = body["stream_options"].as_object_mut() { o.remove("include_usage"); } if body["stream_options"].as_object().map(|o| o.is_empty()).unwrap_or(false) { if let Some(o) = body.as_object_mut() { o.remove("stream_options"); } } continue; }
            break (st, hs, Up::Mem(Some(Bytes::from(bytes))));
        }
        break (st, hs, up);
    };
    let ct = ct_of(&rheaders); let out_h = oa_response_headers(&rheaders); // with the chain's switches (a Work run's relay puts them in the transcript)
    if !(200..300).contains(&status) {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(Fmt::Oa, 499, "cancelled") };
        return raw(status, &ct, &out_h, pgm_net_internal::http::full(bytes)); // the provider's own error, as it came
    }
    if !stream {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(Fmt::Oa, 499, "cancelled") };
        let ur = OaUsage::from_json(&bytes); note_native(hub, user, key, &t, &ur.model, &ur.native);
        drop(run);
        return raw(status, &ct, &out_h, pgm_net_internal::http::full(bytes));
    }
    if ct.to_ascii_lowercase().contains("text/event-stream") { return streamed(hub, user, key, &t, up, Box::new(RawOa(OaUsage::default())), run, status, &ct, &out_h); }
    let Some(bytes) = read_all(up, &cancel).await else { return err_resp(Fmt::Oa, 499, "cancelled") };
    let j: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !j["choices"].is_array() { drop(run); return raw(status, &ct, &out_h, pgm_net_internal::http::full(bytes)); } // not a completion: as it is
    if !asked_usage { let ur = OaUsage::from_json(&bytes); note_native(hub, user, key, &t, &ur.model, &ur.native); } // the stream below will not carry the usage
    let sse = crate::xlate::oa_whole_as_sse(&j, asked_usage);
    streamed(hub, user, key, &t, Up::Mem(Some(Bytes::from(sse))), Box::new(RawOa(OaUsage::default())), run, 200, "text/event-stream", &out_h)
}

/// `POST /v1/chat/completions` to an Anthropic-speaking provider (G3): the OpenAI request is translated to Messages
/// ([`crate::xlate::oa_to_ant_request`]: system, images, tools, tool_choice, stop, max_tokens), and the answer comes back as a
/// `chat.completion` or, streamed, as `chat.completion.chunk` events with the calls in `delta.tool_calls` (arguments as they
/// arrive), `finish_reason` mapped (end_turn to stop, tool_use to tool_calls, max_tokens to length) and, when the caller asked
/// (`stream_options.include_usage`), the usage. What cannot be carried is a 400 naming it. A Claude subscription login's
/// refusal is the plain message of [`SUBSCRIPTION_MESSAGE`]; the request is sent as it is, never as Claude Code.
pub async fn chat_via_anthropic(hub: &Arc<dyn AiHub>, r: &Req, user: &str, key: &openai::KeyCtx, prov: &Value, model: &str, b: Value) -> Resp {
    let f = Fmt::Oa;
    let req = match crate::xlate::oa_to_ant_request(&b, model) { Ok(x) => x, Err(m) => return err_resp(f, 400, &m) };
    let p = full_provider(hub, user, prov);
    let t = tally(&p, model);
    if let Some(e) = preflight(hub, user, &p, f) { return e; }
    let Some(run) = openai::start_running(hub, user, key) else { return openai::too_many(false, key, openai::max_concurrent(&**hub, key)) };
    count_call(hub, user, key, &t);
    let stream = b["stream"].as_bool().unwrap_or(false);
    let cancel = run.cancel.clone();
    let (status, rheaders, up) = match ant_upstream(hub, user, &p, request_headers(&r.headers, stream), serde_json::to_vec(&req).unwrap_or_default(), &cancel, f).await { Ok(x) => x, Err(e) => return e };
    let out_h = response_headers(&rheaders);
    let (cid, created) = (format!("chatcmpl-{}", pgm_core_internal::crypto::random_hex(12)), pgm_core_internal::now_ms() / 1000);
    if !(200..300).contains(&status) {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        if is_login(&p) && is_subscription_refusal(status, &rheaders, &bytes) { return err_resp(f, 403, SUBSCRIPTION_MESSAGE); }
        return with_retry(err_resp(f, status, &upstream_message(status, &bytes)), &rheaders);
    }
    if !stream {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        let Ok(j) = serde_json::from_slice::<Value>(&bytes) else { return err_resp(f, 502, "Anthropic's answer was not JSON.") };
        let ur = UsageReader::from_json(&bytes); note_usage(hub, user, key, &t, &ur);
        drop(run);
        return openai::cors(pgm_net_internal::http::json(200, &crate::xlate::ant_to_oa_whole(&j, &cid, created, model)));
    }
    let pump = AntToOaPump(crate::xlate::AntToOa::new(&cid, model, created, b["stream_options"]["include_usage"] == json!(true)));
    streamed(hub, user, key, &t, up, Box::new(pump), run, 200, "text/event-stream", &out_h)
}

/// `POST /v1/messages` to an OpenAI-speaking provider (G3): the Anthropic request is translated to chat completions
/// ([`crate::xlate::ant_to_oa_request`]: system, images, tools and tool_use / tool_result turns, tool_choice,
/// stop_sequences, max_tokens), and the answer comes back as an Anthropic message or, streamed, as Anthropic SSE events
/// (`message_start`, text and tool_use content blocks with `input_json_delta`, `message_delta` with the stop reason and the
/// usage, `message_stop`). What cannot be carried (documents, server tools, MCP servers ...) is a 400 naming it; `thinking` and
/// `cache_control` are dropped.
pub async fn messages_via_openai(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, prov: &Value, model: &str, b: Value) -> Resp {
    let f = Fmt::Ant;
    let req = match crate::xlate::ant_to_oa_request(&b, model) { Ok(x) => x, Err(m) => return err_resp(f, 400, &m) };
    let p = full_provider(hub, user, prov);
    let t = tally(&p, model);
    if let Some(e) = preflight(hub, user, &p, f) { return e; }
    let Some(run) = openai::start_running(hub, user, key) else { return openai::too_many(true, key, openai::max_concurrent(&**hub, key)) };
    count_call(hub, user, key, &t);
    let stream = b["stream"].as_bool().unwrap_or(false);
    let cancel = run.cancel.clone();
    let (status, rheaders, up) = match oa_upstream(hub, user, &p, CHAT, serde_json::to_vec(&req).unwrap_or_default(), stream, &cancel, f).await { Ok(x) => x, Err(e) => return e };
    let mid = format!("msg_{}", pgm_core_internal::crypto::random_hex(12));
    if !(200..300).contains(&status) {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        return with_retry(err_resp(f, status, &upstream_message(status, &bytes)), &rheaders);
    }
    let out_h = oa_response_headers(&rheaders);
    if !stream {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        let Ok(j) = serde_json::from_slice::<Value>(&bytes) else { return err_resp(f, 502, "The endpoint's answer was not JSON.") };
        let ur = OaUsage::from_json(&bytes); note_native(hub, user, key, &t, &ur.model, &ur.native);
        drop(run);
        return with_switch(openai::cors(pgm_net_internal::http::json(200, &crate::xlate::oa_to_ant_whole(&j, &mid, model))), &out_h);
    }
    let up = if ct_of(&rheaders).to_ascii_lowercase().contains("text/event-stream") { up } else { // an endpoint that answers JSON to a streaming request
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        let j: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !j["choices"].is_array() { return err_resp(f, 502, "The endpoint answered a streaming request with something that is not a chat completion."); }
        Up::Mem(Some(Bytes::from(crate::xlate::oa_whole_as_sse(&j, true))))
    };
    streamed(hub, user, key, &t, up, Box::new(OaToAntPump(crate::xlate::OaToAnt::new(&mid, model))), run, 200, "text/event-stream", &out_h)
}

// ---------- G5: the Responses API (Codex) ----------

/// Does this provider speak OpenAI's Responses API itself? An OpenAI-compatible endpoint at api.openai.com, or one its admin
/// marked `responses: true` (the optional provider field, add-only in providers.json; AI Admin → My AIs or
/// `/api/providers/add|update`). A host's ChatGPT / Codex login (`host-codex`) speaks it too ([`responses`] takes it apart).
/// A host's own OpenAI-compatible server (`host-openai`) is not asked: it gets the translation.
pub fn speaks_responses(p: &Value) -> bool {
    if s(p, "kind") != "openai" { return false; }
    if p["responses"] == json!(true) { return true; }
    let u = s(p, "baseUrl").to_ascii_lowercase();
    u.split("://").nth(1).unwrap_or("").split(['/', ':']).next().unwrap_or("") == "api.openai.com"
}
fn resp_id() -> (String, u64) { (format!("resp_{}", pgm_core_internal::crypto::random_hex(16)), pgm_core_internal::now_ms() / 1000) }

/// `POST /v1/responses` (OpenAI's Responses API, what Codex speaks with `wire_api = "responses"`), routed by `model` like the
/// other endpoints. State this hub does not keep (`previous_response_id`, `conversation`, `background`, `item_reference`) is
/// refused with a 400 whatever the provider. Then:
/// - a provider that speaks Responses itself ([`speaks_responses`], and a host's ChatGPT / Codex login) gets the request raw
///   ([`responses_native`]) and its answer comes back byte for byte;
/// - another OpenAI-speaking provider gets it as chat completions ([`responses_via_chat`]);
/// - an Anthropic-speaking one as Messages ([`responses_via_anthropic`]; a Claude subscription login's refusal is the plain
///   message of [`SUBSCRIPTION_MESSAGE`]);
///
/// and a translated answer comes back as the Responses API's events or object ([`crate::xlate::RespStream`]). Providers that
/// speak neither (this hub's own Claude, a Claude Code login) are refused with a 400 that says where they answer.
pub async fn responses(hub: &Arc<dyn AiHub>, r: &Req, user: &str, key: &openai::KeyCtx, prov: &Value, model: &str, b: Value) -> Resp {
    if let Err(m) = crate::xlate::responses_stateless(&b) { return err_resp(Fmt::Oa, 400, &m); }
    let p = full_provider(hub, user, prov);
    if s(&p, "kind") == "host-codex" || speaks_responses(&p) { return responses_native(hub, user, key, &p, model, b).await; }
    if speaks_openai(&p) { return responses_via_chat(hub, user, key, &p, model, b).await; }
    if speaks_anthropic(&p) { return responses_via_anthropic(hub, r, user, key, &p, model, b).await; }
    err_resp(Fmt::Oa, 400, &format!("The provider \"{}\" cannot answer the Responses API: /v1/responses reaches OpenAI-compatible, Anthropic and ChatGPT / Codex providers. Ask it through /v1/chat/completions or /v1/messages, or pick another model.", s(&p, "label")))
}
/// Raw to a provider that speaks Responses: the body as parsed with `model` swapped and `store: false` (this hub keeps nothing
/// and refuses `previous_response_id`, so nothing is stored upstream either; a ChatGPT / Codex login takes nothing else), the
/// provider's own auth (an endpoint's bearer, or the host's Codex login added on the host), the answer as it came: SSE byte for
/// byte, JSON as sent, errors with their status. A ChatGPT / Codex login answers streams only: a whole answer asked of it is
/// made from its stream (the response of its `response.completed`). The usage goes to the ledger and the key's counters.
async fn responses_native(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, p: &Value, model: &str, b: Value) -> Resp {
    let f = Fmt::Oa;
    let t = tally(p, model);
    let codex = s(p, "kind") == "host-codex";
    if let Some(e) = preflight(hub, user, p, f) { return e; }
    let Some(run) = openai::start_running(hub, user, key) else { return openai::too_many(false, key, openai::max_concurrent(&**hub, key)) };
    count_call(hub, user, key, &t);
    let stream = b["stream"] == json!(true);
    let mut body = b;
    if !model.is_empty() { body["model"] = json!(model); }
    body["store"] = json!(false);
    if codex { body["stream"] = json!(true); }
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    let cancel = run.cancel.clone();
    let (status, rheaders, up) = if codex {
        let (host, pid) = (s(p, "hostId"), s(p, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default());
        let headers = vec![("content-type".to_string(), "application/json".to_string()), ("accept".into(), "text/event-stream".into())];
        let sent = tokio::select! { _ = cancel.woke() => return err_resp(f, 499, "cancelled"), x = hub.ai().login_fetch_for(user, "api", &host, &pid, "/responses", "POST", headers, bytes) => x };
        match sent { Ok(x) => (x.status, x.headers, Up::Host(x.body)), Err(e) => return err_resp(f, e.status, &e.msg) }
    } else {
        match oa_upstream(hub, user, p, "/responses", bytes, stream, &cancel, f).await { Ok(x) => x, Err(e) => return e }
    };
    let ct = ct_of(&rheaders); let out_h = oa_response_headers(&rheaders);
    if !(200..300).contains(&status) {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        return raw(status, &ct, &out_h, pgm_net_internal::http::full(bytes)); // the provider's own error, as it came
    }
    if !stream {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        if !codex && !ct.to_ascii_lowercase().contains("text/event-stream") {
            let rr = crate::xlate::RespReader::from_json(&bytes); note_native(hub, user, key, &t, &rr.model, &rr.native);
            drop(run);
            return raw(status, &ct, &out_h, pgm_net_internal::http::full(bytes));
        }
        let mut rr = crate::xlate::RespReader::default(); rr.push(&bytes); rr.push(b"\n\n");
        note_native(hub, user, key, &t, &rr.model, &rr.native);
        drop(run);
        return match rr.done {
            Some(x) if x["status"] == json!("failed") => err_resp(f, 502, x["error"]["message"].as_str().unwrap_or("The provider's answer failed.")),
            Some(x) => openai::cors(pgm_net_internal::http::json(200, &x)),
            None => err_resp(f, 502, "The provider's stream ended before its response was complete."),
        };
    }
    streamed(hub, user, key, &t, up, Box::new(RawResp(crate::xlate::RespReader::default())), run, status, if ct.is_empty() { "text/event-stream" } else { &ct }, &out_h)
}
/// To an OpenAI-speaking provider that does not speak Responses: as chat completions ([`crate::xlate::responses_to_chat`]),
/// the answer back as a response object or Responses events; what cannot be carried is a 400 naming it.
async fn responses_via_chat(hub: &Arc<dyn AiHub>, user: &str, key: &openai::KeyCtx, p: &Value, model: &str, b: Value) -> Resp {
    let f = Fmt::Oa;
    let req = match crate::xlate::responses_to_chat(&b, model) { Ok(x) => x, Err(m) => return err_resp(f, 400, &m) };
    let t = tally(p, model);
    if let Some(e) = preflight(hub, user, p, f) { return e; }
    let Some(run) = openai::start_running(hub, user, key) else { return openai::too_many(false, key, openai::max_concurrent(&**hub, key)) };
    count_call(hub, user, key, &t);
    let stream = b["stream"] == json!(true);
    let cancel = run.cancel.clone();
    let (status, rheaders, up) = match oa_upstream(hub, user, p, CHAT, serde_json::to_vec(&req).unwrap_or_default(), stream, &cancel, f).await { Ok(x) => x, Err(e) => return e };
    if !(200..300).contains(&status) {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        return with_retry(err_resp(f, status, &upstream_message(status, &bytes)), &rheaders);
    }
    let out_h = oa_response_headers(&rheaders);
    let (rid, created) = resp_id();
    let em = crate::xlate::RespStream::new(&rid, created, model, crate::xlate::response_base(&b));
    if !stream {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        let Ok(j) = serde_json::from_slice::<Value>(&bytes) else { return err_resp(f, 502, "The endpoint's answer was not JSON.") };
        let ur = OaUsage::from_json(&bytes); note_native(hub, user, key, &t, &ur.model, &ur.native);
        drop(run);
        return with_switch(openai::cors(pgm_net_internal::http::json(200, &crate::xlate::chat_to_response_whole(&j, em))), &out_h);
    }
    let up = if ct_of(&rheaders).to_ascii_lowercase().contains("text/event-stream") { up } else { // an endpoint that answers JSON to a streaming request
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        let j: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !j["choices"].is_array() { return err_resp(f, 502, "The endpoint answered a streaming request with something that is not a chat completion."); }
        Up::Mem(Some(Bytes::from(crate::xlate::oa_whole_as_sse(&j, true))))
    };
    streamed(hub, user, key, &t, up, Box::new(ChatToRespPump(crate::xlate::ChatToResp::new(em))), run, 200, "text/event-stream", &out_h)
}
/// To an Anthropic-speaking provider (an API key, a host's Claude login): as Messages ([`crate::xlate::responses_to_ant`]),
/// the answer back as a response object or Responses events. The login is sent the request as it is (never as Claude Code);
/// its subscription refusal is the plain message of [`SUBSCRIPTION_MESSAGE`].
async fn responses_via_anthropic(hub: &Arc<dyn AiHub>, r: &Req, user: &str, key: &openai::KeyCtx, p: &Value, model: &str, b: Value) -> Resp {
    let f = Fmt::Oa;
    let req = match crate::xlate::responses_to_ant(&b, model) { Ok(x) => x, Err(m) => return err_resp(f, 400, &m) };
    let t = tally(p, model);
    if let Some(e) = preflight(hub, user, p, f) { return e; }
    let Some(run) = openai::start_running(hub, user, key) else { return openai::too_many(false, key, openai::max_concurrent(&**hub, key)) };
    count_call(hub, user, key, &t);
    let stream = b["stream"] == json!(true);
    let cancel = run.cancel.clone();
    let (status, rheaders, up) = match ant_upstream(hub, user, p, request_headers(&r.headers, stream), serde_json::to_vec(&req).unwrap_or_default(), &cancel, f).await { Ok(x) => x, Err(e) => return e };
    if !(200..300).contains(&status) {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        if is_login(p) && is_subscription_refusal(status, &rheaders, &bytes) { return err_resp(f, 403, SUBSCRIPTION_MESSAGE); }
        return with_retry(err_resp(f, status, &upstream_message(status, &bytes)), &rheaders);
    }
    let out_h = response_headers(&rheaders);
    let (rid, created) = resp_id();
    let em = crate::xlate::RespStream::new(&rid, created, model, crate::xlate::response_base(&b));
    if !stream {
        let Some(bytes) = read_all(up, &cancel).await else { return err_resp(f, 499, "cancelled") };
        let Ok(j) = serde_json::from_slice::<Value>(&bytes) else { return err_resp(f, 502, "Anthropic's answer was not JSON.") };
        note_usage(hub, user, key, &t, &UsageReader::from_json(&bytes));
        drop(run);
        return openai::cors(pgm_net_internal::http::json(200, &crate::xlate::ant_to_response_whole(&j, em)));
    }
    streamed(hub, user, key, &t, up, Box::new(AntToRespPump(crate::xlate::AntToResp::new(em))), run, 200, "text/event-stream", &out_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn which_providers_speak_responses() {
        assert!(speaks_responses(&json!({ "kind": "openai", "baseUrl": "https://api.openai.com/v1" })));
        assert!(speaks_responses(&json!({ "kind": "openai", "baseUrl": "http://127.0.0.1:9/oai", "responses": true })));
        assert!(!speaks_responses(&json!({ "kind": "openai", "baseUrl": "https://api.openai.com.evil.test/v1" })));
        assert!(!speaks_responses(&json!({ "kind": "openai", "baseUrl": "http://localhost:11434/v1" })));
        for k in ["host-openai", "anthropic", "host-claude", "host-codex", "host"] { assert!(!speaks_responses(&json!({ "kind": k, "responses": true, "baseUrl": "https://api.openai.com/v1" })), "{k}"); }
    }
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
