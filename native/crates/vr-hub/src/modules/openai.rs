//! The external API (moved from bridge/openai.js and bridge/providers.js): OpenAI- and Anthropic-compatible, one key
//! per person, both formats on every provider, and the providers themselves (Settings → AI providers).
//!   GET  /v1/models                  the models of the key's provider (Anthropic's list shape with anthropic-version)
//!   POST /v1/chat/completions        OpenAI chat format in (stream or not)
//!   POST /v1/messages                Anthropic Messages format in (stream or not: Anthropic's SSE events)
//!   POST /v1/messages/count_tokens   a rough count (about 4 characters a token)
//!   Authorization: Bearer mills_… or x-api-key: mills_…   (made in Settings → API access)
//! Keys: bridge/api-keys.json { <username>: { hash (sha256), prefix, createdAt, expiresDays, calls, lastUsed, provider,
//! model } }, taken over from each account's state.json ("api") the first time. Providers: bridge/providers.json.
use bytes::Bytes;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Notify};

use super::auth::Account;
use super::{answer, Routed};
use crate::front::{Hub, Req};
use vr_core::now_ms;

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const NO_AI: &str = "No AI is available: this hub has no Claude of its own and no AI host offering one is connected.";

#[derive(Debug)]
pub struct ApiErr { pub status: u16, pub msg: String }
fn err(status: u16, m: impl Into<String>) -> ApiErr { ApiErr { status, msg: m.into() } }
/// One answer being made: cancelled when its caller hangs up.
#[derive(Default)]
pub struct Cancel { on: AtomicBool, woke: Notify }
impl Cancel {
    pub fn fire(&self) { self.on.store(true, Ordering::SeqCst); self.woke.notify_waiters(); }
    pub fn is(&self) -> bool { self.on.load(Ordering::SeqCst) }
    /// Resolves once cancelled.
    pub async fn woke(&self) { let n = self.woke.notified(); tokio::pin!(n); n.as_mut().enable(); if self.is() { return; } n.await }
}
pub struct Answer { text: String, model: String, usage: Value }

pub struct Api { root: PathBuf, mills: PathBuf, keys: Mutex<Map<String, Value>>, running: Mutex<HashMap<String, Arc<Cancel>>>, http: reqwest::Client }
fn read_json(p: &Path) -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) }
fn write_json(p: &Path, v: &Value) { let tmp = p.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() { vr_core::settings::private(&tmp); let _ = std::fs::rename(&tmp, p); } }
fn s(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_string() }
fn js_len(t: &str) -> usize { t.encode_utf16().count() }

impl Api {
    pub fn new(root: &Path, accounts: &[Account]) -> Api {
        let f = root.join("api-keys.json");
        let keys = match read_json(&f).and_then(|v| v.as_object().cloned()) {
            Some(m) => m,
            None => { let mut m = Map::new(); for a in accounts { if let Some(api) = read_json(&a.dir.join("state.json")).map(|st| st["api"].clone()).filter(Value::is_object) { m.insert(a.username.clone(), api); } } write_json(&f, &Value::Object(m.clone())); m }
        };
        // (no redirects: an endpoint someone typed in must not lead this hub on to an address of its own choosing)
        let _ = vr_core::hub::http_client(std::time::Duration::from_secs(15));
        let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15)).redirect(reqwest::redirect::Policy::none()).build().expect("http");
        Api { root: root.to_path_buf(), mills: root.join(".."), keys: Mutex::new(keys), running: Mutex::default(), http }
    }
    fn save(&self) { write_json(&self.root.join("api-keys.json"), &Value::Object(self.keys.lock().unwrap().clone())); }
    fn key_of(&self, u: &str) -> Value { self.keys.lock().unwrap().get(u).cloned().unwrap_or(json!({})) }
    fn set_key(&self, u: &str, v: Option<Value>) { { let mut k = self.keys.lock().unwrap(); match v { Some(v) => { k.insert(u.into(), v); } None => { k.remove(u); } } } self.save(); }
    pub fn busy(&self) -> bool { !self.running.lock().unwrap().is_empty() }
    pub fn stop(&self, u: &str) { if let Some(c) = self.running.lock().unwrap().get(u) { c.fire(); } }
    pub fn drop_provider(&self, id: &str) {
        let changed = { let mut k = self.keys.lock().unwrap(); let mut ch = false; for v in k.values_mut() { if v["provider"] == json!(id) { if let Some(o) = v.as_object_mut() { o.remove("provider"); ch = true; } } } ch };
        if changed { self.save(); }
    }
    fn status(&self, u: &str) -> Value {
        let a = self.key_of(u);
        let days = a["expiresDays"].as_u64().unwrap_or(0);
        let created = chrono::DateTime::parse_from_rfc3339(a["createdAt"].as_str().unwrap_or("")).ok().map(|d| d.timestamp_millis() as u64);
        let expires = match (created, days) { (Some(c), d) if d > 0 => Some(c + d * 86_400_000), _ => None };
        let tunnel = read_json(&self.root.join("tunnel.json")).map(|t| s(&t, "url")).unwrap_or_default();
        json!({ "hasKey": !s(&a, "hash").is_empty(), "prefix": s(&a, "prefix"), "createdAt": a.get("createdAt").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null), "expiresDays": days, "expiresAt": expires, "expired": expires.map(|e| e < now_ms()).unwrap_or(false),
            "provider": if s(&a, "provider").is_empty() { "host".into() } else { s(&a, "provider") }, "model": s(&a, "model"), "endpoint": format!("{tunnel}/v1"), "calls": a["calls"].as_u64().unwrap_or(0), "lastUsed": a.get("lastUsed").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null) })
    }
    fn account_for_key(&self, r: &Req) -> Result<String, String> {
        let k = r.header("authorization").and_then(|v| v.strip_prefix("Bearer ")).map(|v| v.trim().to_string()).unwrap_or_else(|| r.header("x-api-key").unwrap_or("").trim().to_string());
        if !(k.len() == 54 && k.starts_with("mills_") && k[6..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())) { return Err("Missing or malformed API key. Use \"Authorization: Bearer mills_…\" (or \"x-api-key: mills_…\") with the key from Settings → API access.".into()); }
        let h = vr_core::crypto::sha256_hex(k.as_bytes());
        let found = self.keys.lock().unwrap().iter().find(|(_, v)| super::work::same_secret(v["hash"].as_str().unwrap_or(""), &h)).map(|(u, _)| u.clone());
        let Some(u) = found else { return Err("Unknown API key (rotated, revoked, or never issued).".into()) };
        if self.status(&u)["expired"] == json!(true) { return Err("This API key has expired; rotate it in Settings → API access.".into()); }
        Ok(u)
    }
}
fn clamp_days(d: &Value) -> u64 { let n = d.as_f64().or_else(|| d.as_str().and_then(|x| x.parse().ok())).unwrap_or(0.0).round() as i64; if [0, 1, 7, 30, 90].contains(&n) { n as u64 } else { 0 } }

// ---------- providers (Settings → AI providers): bridge/providers.json ----------
fn providers_file(hub: &Hub) -> PathBuf { hub.root.join("providers.json") }
fn providers_db(hub: &Hub) -> Vec<Value> { read_json(&providers_file(hub)).and_then(|v| v["providers"].as_array().cloned()).unwrap_or_default() }
fn save_providers(hub: &Hub, list: Vec<Value>) { write_json(&providers_file(hub), &json!({ "providers": list })); }
fn host_models(hub: &Hub) -> Vec<String> { hub.cfg.get("models").and_then(Value::as_array).map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default() }
fn clean_models(m: &Value, fallback: Vec<String>) -> Vec<String> {
    let list: Vec<String> = match m { Value::Array(a) => a.iter().map(|x| match x { Value::String(t) => t.trim().to_string(), Value::Null => String::new(), v => v.to_string() }).collect(), Value::String(t) => t.split(|c: char| c == ',' || c.is_whitespace()).map(|x| x.trim().to_string()).collect(), Value::Null => vec![], v => vec![v.to_string()] };
    let list: Vec<String> = list.into_iter().filter(|x| !x.is_empty()).take(20).collect();
    if list.is_empty() { fallback } else { list }
}
fn pub_view(p: &Value) -> Value {
    let mut v = json!({ "id": p["id"], "kind": p["kind"], "label": p["label"], "owner": p["owner"], "shared": p["shared"] == json!(true), "models": if p["models"].is_array() { p["models"].clone() } else { json!([]) }, "hasKey": !s(p, "apiKey").is_empty(), "createdAt": p["createdAt"] });
    for k in ["baseUrl", "configDir"] { if !s(p, k).is_empty() { v[k] = p[k].clone(); } }
    v
}
/// A custom endpoint (this hub calls whatever address it names) or a Claude config folder on this machine: admins only.
pub fn admin_only(p: &Value) -> bool { matches!(p["kind"].as_str(), Some("openai") | Some("claude-cli")) }
fn add_provider(hub: &Hub, user: &str, b: &Value) -> Result<Value, String> {
    if admin_only(b) {
        if !hub.mods.auth.is_admin(user) || !hub.mods.ai.owns_online_host(user) { return Err("A custom endpoint or a Claude config folder is for admins signed in on an AI host of their own (Settings → My AI hosts). An Anthropic API key works for everyone.".into()); }
    }
    let kind = s(b, "kind");
    if !["anthropic", "claude-cli", "openai"].contains(&kind.as_str()) { return Err("kind must be anthropic, claude-cli or openai.".into()); }
    let label: String = s(b, "label").trim().chars().take(60).collect();
    if label.is_empty() { return Err("Give it a label.".into()); }
    let mut p = json!({ "id": format!("p-{}", vr_core::crypto::random_hex(5)), "kind": kind, "label": label, "owner": user, "shared": b["shared"].as_bool().unwrap_or(false), "createdAt": super::auth::iso_now(), "models": [] });
    match kind.as_str() {
        "anthropic" => {
            let k = s(b, "apiKey").trim().to_string();
            let ok = k.strip_prefix("sk-ant-").map(|r| r.len() >= 20 && r.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')).unwrap_or(false);
            if !ok { return Err("That does not look like an Anthropic API key (sk-ant-…).".into()); }
            p["apiKey"] = json!(k); p["models"] = json!(clean_models(&b["models"], host_models(hub)));
        }
        "claude-cli" => {
            let d = s(b, "configDir").trim().to_string();
            if d.is_empty() || !Path::new(&d).is_absolute() { return Err("configDir must be an absolute folder on the host (the one you ran `claude login` with CLAUDE_CONFIG_DIR set to).".into()); }
            p["configDir"] = json!(d); p["models"] = json!(clean_models(&b["models"], host_models(hub)));
        }
        _ => {
            let url = s(b, "baseUrl").trim().trim_end_matches('/').to_string();
            let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or("");
            if rest.is_empty() || rest.starts_with('/') || rest.chars().next().map(char::is_whitespace).unwrap_or(true) { return Err("baseUrl must be an http(s) address, e.g. https://api.example.com/v1".into()); }
            p["baseUrl"] = json!(url); p["apiKey"] = json!(s(b, "apiKey").trim());
            let m = clean_models(&b["models"], vec![]);
            if m.is_empty() { return Err("List at least one model id the endpoint serves.".into()); }
            p["models"] = json!(m);
        }
    }
    let mut list = providers_db(hub); list.push(p.clone()); save_providers(hub, list);
    Ok(pub_view(&p))
}
fn update_provider(hub: &Hub, user: &str, id: &str, b: &Value) -> Result<Value, String> {
    let mut list = providers_db(hub);
    let Some(p) = list.iter_mut().find(|x| x["id"] == json!(id)).filter(|x| x["owner"] == json!(user)) else { return Err("Not your provider.".into()) };
    if !b["label"].is_null() { let l: String = match &b["label"] { Value::String(t) => t.trim().chars().take(60).collect(), v => v.to_string() }; if !l.is_empty() { p["label"] = json!(l); } }
    if b.get("shared").is_some() { p["shared"] = json!(!(b["shared"].is_null() || b["shared"] == json!(false) || b["shared"] == json!(0) || b["shared"] == json!(""))); }
    if b.get("models").is_some() { let fb = if p["kind"] == json!("openai") { p["models"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() } else { host_models(hub) }; p["models"] = json!(clean_models(&b["models"], fb)); }
    if !s(b, "apiKey").is_empty() && p["kind"] != json!("claude-cli") { p["apiKey"] = json!(s(b, "apiKey").trim()); }
    let v = pub_view(p); save_providers(hub, list);
    Ok(v)
}
fn remove_provider(hub: &Hub, user: &str, id: &str) -> Result<(), String> {
    let mut list = providers_db(hub);
    let Some(i) = list.iter().position(|x| x["id"] == json!(id)).filter(|i| list[*i]["owner"] == json!(user)) else { return Err("Not your provider.".into()) };
    list.remove(i); save_providers(hub, list);
    // nobody keeps pointing at it
    for a in hub.mods.auth.accounts() { hub.mods.work.drop_provider(&a, id); }
    hub.mods.api.drop_provider(id);
    Ok(())
}
fn pick_model(p: &Value, model: &str) -> String {
    let ms: Vec<String> = p["models"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if !model.is_empty() && ms.iter().any(|m| m == model) { model.into() } else { ms.first().cloned().unwrap_or_else(|| model.to_string()) }
}

// ---------- one completion, whatever the provider ----------
fn flatten(messages: &[(String, String)]) -> String {
    let mut turns: Vec<&(String, String)> = messages.iter().filter(|m| m.0 != "system").collect();
    let last = if turns.last().map(|m| m.0 == "user").unwrap_or(false) { turns.pop() } else { None };
    let hist = turns.iter().map(|m| format!("{}: {}", if m.0 == "assistant" { "Assistant" } else { "User" }, m.1)).collect::<Vec<_>>().join("\n\n");
    (if hist.is_empty() { String::new() } else { format!("Conversation so far:\n\n{hist}\n\n") }) + &match last { Some(l) => format!("{}{}", if hist.is_empty() { "" } else { "User: " }, l.1), None => "Continue.".into() }
}
/// "data: {json}" lines → objects; [DONE] ends it.
#[derive(Default)]
struct Sse { buf: String, done: bool }
impl Sse {
    fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        let mut out = vec![]; if self.done { return out; }
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        while let Some(i) = self.buf.find('\n') {
            let l = self.buf[..i].trim().to_string(); self.buf.drain(..=i);
            let Some(d) = l.strip_prefix("data:") else { continue }; let d = d.trim();
            if d == "[DONE]" { self.done = true; break; }
            if let Ok(v) = serde_json::from_str(d) { out.push(v); }
        }
        out
    }
}
type Tok = Option<mpsc::UnboundedSender<String>>;
fn tok(t: &Tok, x: &str) { if let Some(t) = t { let _ = t.send(x.to_string()); } }
/// A body as it comes, from the internet or from an AI host.
enum Src { Http(reqwest::Response), Host(mpsc::Receiver<Result<Bytes, String>>) }
impl Src { async fn next(&mut self) -> Option<Vec<u8>> { match self { Src::Http(r) => r.chunk().await.ok().flatten().map(|b| b.to_vec()), Src::Host(rx) => match rx.recv().await { Some(Ok(b)) => Some(b.to_vec()), _ => None } } } async fn all(mut self) -> Vec<u8> { let mut v = vec![]; while let Some(c) = self.next().await { v.extend(c); } v } }

async fn complete(hub: &Arc<Hub>, user: &str, provider: &str, model: &str, system: &str, messages: &[(String, String)], on_token: Tok, cancel: Arc<Cancel>, max_tokens: Option<u64>) -> Result<Answer, ApiErr> {
    let w = &hub.mods.work; let ai = &hub.mods.ai;
    let p = (if provider.is_empty() { None } else { w.provider_get(ai, provider, user) }).or_else(|| w.default_for(ai, user)).ok_or_else(|| err(503, NO_AI))?;
    let kind = s(&p, "kind");
    if (kind == "host" || kind == "claude-cli") && hub.root.join("ai-host.off").exists() { return Err(err(503, "The host's AI is switched off right now (its host paused it); an API key or endpoint of your own still answers.")); }
    let m = pick_model(&p, model);
    let streaming = on_token.is_some();
    let chat_msgs = || -> Vec<Value> { messages.iter().filter(|x| x.0 != "system").map(|x| json!({ "role": x.0, "content": x.1 })).collect() };
    let ant_msgs = || -> Vec<Value> { messages.iter().filter(|x| x.0 != "system").map(|x| json!({ "role": if x.0 == "assistant" { "assistant" } else { "user" }, "content": x.1 })).collect() };
    let host = s(&p, "hostId"); let pid = s(&p, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default(); let nick = s(&p, "nickname");
    let fail_text = |who: String, status: u16, body: Vec<u8>, keep429: bool| err(if keep429 && status == 429 { 429 } else { 502 }, format!("{who} answered {status}: {}", String::from_utf8_lossy(&body).chars().take(300).collect::<String>()));
    match kind.as_str() {
        "openai" | "host-openai" => {
            let mut body = json!({ "model": if m.is_empty() { p["models"][0].clone() } else { json!(m) }, "stream": streaming, "messages": [] });
            let mut all = vec![]; if !system.is_empty() { all.push(json!({ "role": "system", "content": system })); } all.extend(chat_msgs()); body["messages"] = json!(all);
            if let Some(mt) = max_tokens { body["max_tokens"] = json!(mt); }
            let (status, ct, src) = if kind == "openai" {
                let mut rb = self_http(hub).post(format!("{}/chat/completions", s(&p, "baseUrl"))).header("content-type", "application/json").body(body.to_string());
                if !s(&p, "apiKey").is_empty() { rb = rb.header("authorization", format!("Bearer {}", s(&p, "apiKey"))); }
                let r = rb.send().await.map_err(|e| err(502, e.to_string()))?;
                (r.status().as_u16(), r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string(), Src::Http(r))
            } else {
                let r = ai.provider_fetch(&host, &pid, "/chat/completions", "POST", vec![("content-type".into(), "application/json".into())], body.to_string().into_bytes()).await.map_err(|e| err(e.status, e.msg))?;
                (r.status, r.headers.iter().find(|(k, _)| k == "content-type").map(|x| x.1.clone()).unwrap_or_default(), Src::Host(r.body))
            };
            let who = if kind == "openai" { "The endpoint".to_string() } else { format!("{nick}'s endpoint") };
            if !(200..300).contains(&status) { return Err(fail_text(who, status, src.all().await, false)); }
            let used = s(&body, "model");
            if !streaming || (kind == "host-openai" && !ct.to_lowercase().contains("text/event-stream")) {
                let j: Value = serde_json::from_slice(&src.all().await).unwrap_or(Value::Null);
                let text = j["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
                if streaming && !text.is_empty() { tok(&on_token, &text); }
                return Ok(Answer { text, model: j["model"].as_str().map(String::from).unwrap_or(used), usage: j["usage"].clone() });
            }
            let (mut text, mut used, mut usage, mut sse, mut src) = (String::new(), used, Value::Null, Sse::default(), src);
            while let Some(c) = next_or_cancel(&mut src, &cancel).await? { for ev in sse.push(&c) {
                if let Some(d) = ev["choices"][0]["delta"]["content"].as_str() { text.push_str(d); tok(&on_token, d); }
                if let Some(x) = ev["model"].as_str() { used = x.into(); } if !ev["usage"].is_null() { usage = ev["usage"].clone(); }
            } if sse.done { break; } }
            Ok(Answer { text, model: used, usage })
        }
        "anthropic" | "host-claude" if !(kind == "host-claude" && s(&p, "remoteKind") == "claude-cli-work") => {
            let mut body = json!({ "model": if m.is_empty() { p["models"][0].clone() } else { json!(m) }, "max_tokens": max_tokens.unwrap_or(4096), "stream": streaming, "messages": ant_msgs() });
            let (status, src) = if kind == "anthropic" {
                if !system.is_empty() { body["system"] = json!(system); }
                let r = self_http(hub).post(ANTHROPIC_URL).header("content-type", "application/json").header("x-api-key", s(&p, "apiKey")).header("anthropic-version", "2023-06-01").body(body.to_string()).send().await.map_err(|e| err(502, e.to_string()))?;
                (r.status().as_u16(), Src::Http(r))
            } else {
                // a subscription login answers only when the system prompt opens as Claude Code
                let mut sys = vec![json!({ "type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude." })]; if !system.is_empty() { sys.push(json!({ "type": "text", "text": system })); }
                body["system"] = json!(sys);
                let r = ai.login_fetch(&host, &pid, "/v1/messages", "POST", vec![("content-type".into(), "application/json".into()), ("anthropic-version".into(), "2023-06-01".into())], body.to_string().into_bytes()).await.map_err(|e| err(e.status, e.msg))?;
                (r.status, Src::Host(r.body))
            };
            if !(200..300).contains(&status) { return Err(if kind == "anthropic" { err(502, format!("Anthropic answered {status}: {}", String::from_utf8_lossy(&src.all().await).chars().take(300).collect::<String>())) } else { fail_text(format!("{nick}'s Claude"), status, src.all().await, true) }); }
            if !streaming {
                let j: Value = serde_json::from_slice(&src.all().await).unwrap_or(Value::Null);
                let text = j["content"].as_array().map(|c| c.iter().filter_map(|b| b["text"].as_str()).collect::<String>()).unwrap_or_default();
                let usage = if j["usage"].is_object() { json!({ "prompt_tokens": j["usage"]["input_tokens"], "completion_tokens": j["usage"]["output_tokens"] }) } else { Value::Null };
                return Ok(Answer { text, model: s(&j, "model"), usage });
            }
            let (mut text, mut used, mut usage, mut sse, mut src) = (String::new(), s(&body, "model"), Value::Null, Sse::default(), src);
            while let Some(c) = next_or_cancel(&mut src, &cancel).await? { for ev in sse.push(&c) {
                match ev["type"].as_str().unwrap_or("") {
                    "content_block_delta" => if let Some(t) = ev["delta"]["text"].as_str() { text.push_str(t); tok(&on_token, t); },
                    "message_start" => { if let Some(x) = ev["message"]["model"].as_str() { used = x.into(); } usage = json!({ "prompt_tokens": ev["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0), "completion_tokens": 0 }); }
                    "message_delta" => if usage.is_object() && ev["usage"].is_object() { usage["completion_tokens"] = json!(ev["usage"]["output_tokens"].as_u64().unwrap_or(0)); },
                    _ => {}
                }
            } if sse.done { break; } }
            Ok(Answer { text, model: used, usage })
        }
        "host-codex" => {
            let input: Vec<Value> = messages.iter().filter(|x| x.0 != "system").map(|x| { let asst = x.0 == "assistant"; json!({ "role": if asst { "assistant" } else { "user" }, "content": [{ "type": if asst { "output_text" } else { "input_text" }, "text": x.1 }] }) }).collect();
            let body = json!({ "model": if m.is_empty() { p["models"][0].clone() } else { json!(m) }, "instructions": if system.is_empty() { "You are a helpful assistant." } else { system }, "input": input, "stream": true, "store": false });
            let r = ai.login_fetch(&host, &pid, "/responses", "POST", vec![("content-type".into(), "application/json".into()), ("accept".into(), "text/event-stream".into())], body.to_string().into_bytes()).await.map_err(|e| err(e.status, e.msg))?;
            if !(200..300).contains(&r.status) { let st = r.status; return Err(fail_text(format!("{nick}'s Codex"), st, Src::Host(r.body).all().await, true)); }
            let (mut text, mut used, mut usage, mut sse, mut src) = (String::new(), s(&body, "model"), Value::Null, Sse::default(), Src::Host(r.body));
            while let Some(c) = next_or_cancel(&mut src, &cancel).await? { for ev in sse.push(&c) {
                match ev["type"].as_str().unwrap_or("") {
                    "response.output_text.delta" => if let Some(d) = ev["delta"].as_str().filter(|d| !d.is_empty()) { text.push_str(d); tok(&on_token, d); },
                    "response.completed" => { if let Some(x) = ev["response"]["model"].as_str() { used = x.into(); } let u = &ev["response"]["usage"]; if u.is_object() { usage = json!({ "prompt_tokens": u["input_tokens"].as_u64().unwrap_or(0), "completion_tokens": u["output_tokens"].as_u64().unwrap_or(0) }); } }
                    _ => {}
                }
            } if sse.done { break; } }
            Ok(Answer { text, model: used, usage })
        }
        "host-claude" => { // legacy: a host that allows work runs a sandboxed `claude -p` there
            let (id, _t, mut rx) = ai.run_work(&host, &pid, json!({ "sandbox": true, "account": "api", "display": "API", "message": flatten(messages), "systemPrompt": system, "model": m, "hubUrl": "" })).map_err(|e| err(503, e.msg))?;
            let (mut text, mut used, mut usage, mut er) = (String::new(), m.clone(), Value::Null, String::new());
            loop {
                tokio::select! {
                    _ = cancel.woke.notified() => { ai.cancel_work(&id); return Err(err(502, "cancelled")); }
                    msg = rx.recv() => match msg {
                        Some(super::ai_hosts::WorkMsg::Event(ev)) => claude_event(&ev, &mut text, &mut used, &mut usage, &mut er, &on_token),
                        Some(super::ai_hosts::WorkMsg::Done(_)) | None => break,
                        Some(super::ai_hosts::WorkMsg::Error(e)) => return Err(err(503, e)),
                    }
                }
            }
            if text.is_empty() { return Err(err(502, if er.trim().is_empty() { "the host's Claude gave no answer".to_string() } else { er.trim().to_string() })); }
            Ok(Answer { text, model: used, usage: cli_usage(&usage) })
        }
        _ => complete_cli(hub, &p, &m, system, messages, on_token, cancel).await, // this hub's Claude, or a Claude Code login here
    }
}
fn self_http(hub: &Hub) -> &reqwest::Client { &hub.mods.api.http }
async fn next_or_cancel(src: &mut Src, cancel: &Cancel) -> Result<Option<Vec<u8>>, ApiErr> {
    if cancel.is() { return Err(err(502, "cancelled")); }
    tokio::select! { _ = cancel.woke.notified() => Err(err(502, "cancelled")), c = src.next() => Ok(c) }
}
fn claude_event(ev: &Value, text: &mut String, used: &mut String, usage: &mut Value, er: &mut String, on_token: &Tok) {
    match ev["type"].as_str().unwrap_or("") {
        "system" => if let Some(m) = ev["model"].as_str() { *used = m.into(); },
        "assistant" if ev["message"].is_object() => {
            if let Some(m) = ev["message"]["model"].as_str() { *used = m.into(); }
            if ev["message"]["usage"].is_object() { *usage = ev["message"]["usage"].clone(); }
            for b in ev["message"]["content"].as_array().cloned().unwrap_or_default() { if b["type"] == json!("text") { if let Some(t) = b["text"].as_str().filter(|t| !t.is_empty()) { text.push_str(t); tok(on_token, t); } } }
        }
        "result" if ev["is_error"] == json!(true) => *er = ev["result"].as_str().or(ev["subtype"].as_str()).unwrap_or("error").to_string(),
        "bridge_error" | "stderr" => { let x = format!("{er} {}", s(ev, "text")); *er = x.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect(); }
        _ => {}
    }
}
fn cli_usage(u: &Value) -> Value {
    if !u.is_object() { return Value::Null; }
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    json!({ "prompt_tokens": n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens"), "completion_tokens": n("output_tokens") })
}
/// `claude -p` in an empty folder (no project files in reach), stateless like the API.
async fn complete_cli(hub: &Arc<Hub>, p: &Value, model: &str, system: &str, messages: &[(String, String)], on_token: Tok, cancel: Arc<Cancel>) -> Result<Answer, ApiErr> {
    let sandbox = hub.mods.api.mills.join("Api").join("sandbox");
    let _ = std::fs::create_dir_all(&sandbox);
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose", "--permission-mode", "default", "--strict-mcp-config"].iter().map(|x| x.to_string()).collect();
    if !system.is_empty() { args.push("--append-system-prompt".into()); args.push(system.into()); }
    if !model.is_empty() { args.push("--model".into()); args.push(model.into()); }
    let command = hub.cfg.get("claudeCommand").and_then(Value::as_str).unwrap_or("claude").to_string();
    let pre: Vec<String> = hub.cfg.get("claudeArgs").and_then(Value::as_array).map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut cmd = tokio::process::Command::new(&command);
    cmd.args(&pre).args(&args).current_dir(&sandbox).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    match s(p, "kind").as_str() { "anthropic" => { cmd.env("ANTHROPIC_API_KEY", s(p, "apiKey")); } "claude-cli" => { cmd.env("CLAUDE_CONFIG_DIR", s(p, "configDir")); } _ => {} }
    #[cfg(windows)]
    { cmd.creation_flags(0x0800_0000); }
    let mut child = cmd.spawn().map_err(|e| err(502, format!("Could not start claude: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() { let inp = flatten(messages); tokio::spawn(async move { let _ = stdin.write_all(inp.as_bytes()).await; let _ = stdin.shutdown().await; }); }
    let er = Arc::new(Mutex::new(String::new()));
    let et = child.stderr.take().map(|e| { let er = er.clone(); tokio::spawn(async move { use tokio::io::AsyncReadExt; let mut e = e; let mut b = vec![0u8; 4096]; while let Ok(n) = e.read(&mut b).await { if n == 0 { break; } let mut x = er.lock().unwrap(); x.push_str(&String::from_utf8_lossy(&b[..n])); let k = x.chars().count(); if k > 600 { *x = x.chars().skip(k - 600).collect(); } } }) });
    let (mut text, mut used, mut usage, mut rerr) = (String::new(), model.to_string(), Value::Null, String::new());
    let mut killed = false;
    if let Some(out) = child.stdout.take() {
        let mut lines = tokio::io::BufReader::new(out).lines();
        loop {
            tokio::select! {
                _ = cancel.woke.notified(), if !killed => { killed = true; if let Some(pid) = child.id() { super::work::kill_pid(pid); } }
                l = lines.next_line() => match l { Ok(Some(l)) => { if let Ok(ev) = serde_json::from_str::<Value>(l.trim()) { claude_event(&ev, &mut text, &mut used, &mut usage, &mut rerr, &on_token); } } _ => break },
            }
        }
    }
    let code = child.wait().await.ok().and_then(|s| s.code());
    if let Some(t) = et { let _ = t.await; }
    if killed || cancel.is() { return Err(err(502, "cancelled")); }
    let e2 = format!("{rerr}{}", er.lock().unwrap());
    if text.is_empty() && (code.map(|c| c != 0).unwrap_or(false) || !e2.is_empty()) { return Err(err(502, if e2.trim().is_empty() { format!("claude exited with code {}", code.map(|c| c.to_string()).unwrap_or_else(|| "null".into())) } else { e2.trim().to_string() })); }
    Ok(Answer { text, model: used, usage: cli_usage(&usage) })
}

// ---------- the API's answers ----------
fn cors(mut r: hyper::Response<crate::front::Body>) -> Routed {
    let h = r.headers_mut();
    h.insert("access-control-allow-origin", hyper::header::HeaderValue::from_static("*"));
    h.insert("access-control-allow-headers", hyper::header::HeaderValue::from_static("Content-Type, Authorization, x-api-key, anthropic-version, anthropic-beta"));
    Routed::Answer(r)
}
fn jout(status: u16, v: Value) -> Routed { cors(crate::front::json(status, &v)) }
fn oa_error(status: u16, m: &str, t: &str) -> Routed { jout(status, json!({ "error": { "message": m, "type": t, "code": Value::Null } })) }
fn ant_error(status: u16, m: &str, t: &str) -> Routed {
    let t = match t { "service_unavailable" => "overloaded_error", "rate_limit_error" | "authentication_error" | "invalid_request_error" | "not_found_error" => t, _ => "api_error" };
    jout(status, json!({ "type": "error", "error": { "type": t, "message": m } }))
}
fn sse_answer(rx: mpsc::UnboundedReceiver<Result<Bytes, String>>) -> Routed {
    use http_body_util::BodyExt;
    let st = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x.map(hyper::body::Frame::data).map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e)), rx)) });
    cors(hyper::Response::builder().status(200).header("content-type", "text/event-stream").header("cache-control", "no-store").header("connection", "keep-alive").header("x-accel-buffering", "no").body(http_body_util::StreamBody::new(st).boxed()).expect("response"))
}
fn content_text(c: &Value) -> String { match c { Value::String(t) => t.clone(), Value::Array(a) => a.iter().map(|x| if x["type"] == json!("text") { s(x, "text") } else { String::new() }).collect(), Value::Null => String::new(), Value::Object(_) => "[object Object]".into(), v => v.to_string() } }
fn block_text(c: &Value) -> String {
    match c {
        Value::String(t) => t.clone(),
        Value::Array(a) => a.iter().filter_map(|b| { if !b.is_object() { return None; } let t = match b["type"].as_str().unwrap_or("") { "text" => s(b, "text"), "tool_result" => block_text(&b["content"]), "tool_use" => format!("[tool call {}: {}]", s(b, "name"), if b["input"].is_null() { "{}".to_string() } else { b["input"].to_string() }.chars().take(2000).collect::<String>()), x @ ("image" | "document") => format!("[{x} not passed on]"), _ => String::new() }; if t.is_empty() { None } else { Some(t) } }).collect::<Vec<_>>().join("\n"),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}
fn from_anthropic(b: &Value) -> (String, Vec<(String, String)>) {
    let system = block_text(&b["system"]);
    let msgs = b["messages"].as_array().map(|a| a.iter().filter(|m| m["role"] == json!("user") || m["role"] == json!("assistant")).map(|m| (s(m, "role"), block_text(&m["content"]))).collect()).unwrap_or_default();
    (system, msgs)
}
/// Marks a person's answer as running until dropped; the caller hanging up cancels it.
struct Running { hub: Arc<Hub>, user: String, cancel: Arc<Cancel> }
impl Drop for Running { fn drop(&mut self) { self.cancel.fire(); self.hub.mods.api.running.lock().unwrap().remove(&self.user); self.hub.mods.work.busy_changed.notify_one(); } }
fn start_running(hub: &Arc<Hub>, user: &str) -> Option<Running> {
    let mut r = hub.mods.api.running.lock().unwrap();
    if r.contains_key(user) { return None; }
    let c = Arc::new(Cancel::default()); r.insert(user.into(), c.clone()); drop(r);
    hub.mods.work.busy_changed.notify_one();
    Some(Running { hub: hub.clone(), user: user.into(), cancel: c })
}
fn count_use(hub: &Hub, u: &str) { let api = &hub.mods.api; let mut k = api.key_of(u); k["calls"] = json!(k["calls"].as_u64().unwrap_or(0) + 1); k["lastUsed"] = json!(super::auth::iso_now()); api.set_key(u, Some(k)); }
fn total(u: &Value) -> Value { let mut v = u.clone(); v["total_tokens"] = json!(u["prompt_tokens"].as_u64().unwrap_or(0) + u["completion_tokens"].as_u64().unwrap_or(0)); v }

/// /v1/*: the API itself (its own keys, no browser session).
pub async fn v1(hub: &Arc<Hub>, r: &Req) -> Routed {
    let p = r.path.as_str();
    if r.method == hyper::Method::OPTIONS { return cors(hyper::Response::builder().status(204).body(crate::front::full("")).expect("response")); }
    let anthropic = p.starts_with("/v1/messages") || (p == "/v1/models" && r.header("anthropic-version").is_some());
    let fail = |st: u16, m: &str, t: &str| if anthropic { ant_error(st, m, t) } else { oa_error(st, m, t) };
    let api = &hub.mods.api;
    let user = match api.account_for_key(r) { Ok(u) => u, Err(e) => return fail(401, &e, "authentication_error") };
    let a = api.key_of(&user);
    let Some(prov) = hub.mods.work.effective(&hub.mods.ai, &user, &s(&a, "provider")) else { return fail(503, "No AI is available: this hub has no Claude of its own and no AI host offering one is connected.", "service_unavailable") };
    let models: Vec<String> = prov["models"].as_array().map(|m| m.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let body: Option<Value> = if r.body.is_empty() { Some(json!({})) } else { serde_json::from_slice(&r.body).ok() };
    match (r.method.as_str(), p) {
        ("GET", "/v1/models") => {
            if anthropic { return jout(200, json!({ "data": models.iter().map(|m| json!({ "type": "model", "id": m, "display_name": m, "created_at": "1970-01-01T00:00:00Z" })).collect::<Vec<_>>(), "has_more": false, "first_id": models.first(), "last_id": models.last() })); }
            jout(200, json!({ "object": "list", "data": models.iter().map(|m| json!({ "id": m, "object": "model", "created": 0, "owned_by": if prov["kind"] == json!("host") { json!("mills") } else { prov["owner"].clone() } })).collect::<Vec<_>>() }))
        }
        ("POST", "/v1/messages/count_tokens") => {
            let Some(b) = body else { return ant_error(400, "Body is not JSON.", "invalid_request_error") };
            let (system, msgs) = from_anthropic(&b);
            let chars = js_len(&system) + msgs.iter().map(|m| js_len(&m.1)).sum::<usize>();
            jout(200, json!({ "input_tokens": chars.div_ceil(4).max(1) }))
        }
        ("POST", "/v1/messages") => {
            let Some(b) = body else { return ant_error(400, "Body is not JSON.", "invalid_request_error") };
            let (system, msgs) = from_anthropic(&b);
            if msgs.is_empty() { return ant_error(400, "messages: at least one user or assistant message is required.", "invalid_request_error"); }
            let Some(run) = start_running(hub, &user) else { return ant_error(429, "One request at a time per key; the previous one is still answering.", "rate_limit_error") };
            let model = pick_model(&prov, &b["model"].as_str().filter(|m| models.iter().any(|x| x == m)).map(String::from).unwrap_or_else(|| s(&a, "model")));
            let max = b["max_tokens"].as_f64().filter(|n| *n > 0.0).map(|n| (n.round() as u64).min(128000));
            count_use(hub, &user);
            let mid = format!("msg_{}", vr_core::crypto::random_hex(12));
            let ant_usage = |u: &Value| json!({ "input_tokens": u["prompt_tokens"].as_u64().unwrap_or(0), "output_tokens": u["completion_tokens"].as_u64().unwrap_or(0) });
            let pid = s(&prov, "id");
            if b["stream"].as_bool().unwrap_or(false) {
                let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, String>>();
                let ev = |tx: &mpsc::UnboundedSender<Result<Bytes, String>>, t: &str, mut d: Value| -> bool { d["type"] = json!(t); let mut o = Map::new(); o.insert("type".into(), json!(t)); if let Some(m) = d.as_object() { for (k, v) in m { if k != "type" { o.insert(k.clone(), v.clone()); } } } tx.send(Ok(Bytes::from(format!("event: {t}\ndata: {}\n\n", Value::Object(o))))).is_ok() };
                ev(&tx, "message_start", json!({ "message": { "id": mid, "type": "message", "role": "assistant", "content": [], "model": model, "stop_reason": Value::Null, "stop_sequence": Value::Null, "usage": { "input_tokens": 0, "output_tokens": 0 } } }));
                ev(&tx, "content_block_start", json!({ "index": 0, "content_block": { "type": "text", "text": "" } }));
                ev(&tx, "ping", json!({}));
                let h = hub.clone();
                tokio::spawn(async move {
                    let (ttx, mut trx) = mpsc::unbounded_channel::<String>();
                    let cancel = run.cancel.clone();
                    let fwd = { let tx = tx.clone(); let cancel = cancel.clone(); tokio::spawn(async move { while let Some(t) = trx.recv().await { if !ev(&tx, "content_block_delta", json!({ "index": 0, "delta": { "type": "text_delta", "text": t } })) { cancel.fire(); } } }) };
                    let res = complete(&h, &user, &pid, &model, &system, &msgs, Some(ttx), cancel, max).await;
                    let _ = fwd.await;
                    match res {
                        Ok(an) => { ev(&tx, "content_block_stop", json!({ "index": 0 })); ev(&tx, "message_delta", json!({ "delta": { "stop_reason": "end_turn", "stop_sequence": Value::Null }, "usage": ant_usage(&an.usage) })); ev(&tx, "message_stop", json!({})); }
                        Err(e) => { let _ = tx.send(Ok(Bytes::from(format!("event: error\ndata: {}\n\n", json!({ "type": "error", "error": { "type": "api_error", "message": e.msg } }))))); }
                    }
                    drop(run);
                });
                return sse_answer(rx);
            }
            let res = complete(hub, &user, &pid, &model, &system, &msgs, None, run.cancel.clone(), max).await;
            drop(run);
            match res {
                Ok(an) => jout(200, json!({ "id": mid, "type": "message", "role": "assistant", "model": if an.model.is_empty() { model } else { an.model }, "content": [{ "type": "text", "text": an.text }], "stop_reason": "end_turn", "stop_sequence": Value::Null, "usage": ant_usage(&an.usage) })),
                Err(e) => ant_error(if e.status == 0 { 502 } else { e.status }, &e.msg, match e.status { 503 => "service_unavailable", 429 => "rate_limit_error", _ => "api_error" }),
            }
        }
        ("POST", "/v1/chat/completions") => {
            let Some(b) = body else { return oa_error(400, "Body is not JSON.", "invalid_request_error") };
            let msgs: Vec<(String, String)> = b["messages"].as_array().map(|a| a.iter().filter(|m| m["role"].is_string()).map(|m| (s(m, "role"), content_text(&m["content"]))).collect()).unwrap_or_default();
            if msgs.is_empty() { return oa_error(400, "\"messages\" is required.", "invalid_request_error"); }
            let Some(run) = start_running(hub, &user) else { return oa_error(429, "One request at a time per key; the previous one is still answering.", "rate_limit_error") };
            let system = msgs.iter().filter(|m| m.0 == "system").map(|m| m.1.clone()).collect::<Vec<_>>().join("\n\n");
            let asked = b["model"].as_str().filter(|m| (m.starts_with("claude") || (!m.is_empty() && m.chars().all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c)))) && models.iter().any(|x| x == m)).map(String::from);
            let model = pick_model(&prov, &asked.unwrap_or_else(|| s(&a, "model")));
            let max = b["max_tokens"].as_u64();
            count_use(hub, &user);
            let cid = format!("chatcmpl-{}", vr_core::crypto::random_hex(12)); let created = now_ms() / 1000;
            let pid = s(&prov, "id");
            if b["stream"].as_bool().unwrap_or(false) {
                let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, String>>();
                let chunk = { let (cid, model) = (cid.clone(), model.clone()); move |tx: &mpsc::UnboundedSender<Result<Bytes, String>>, delta: Value, finish: Value| -> bool { tx.send(Ok(Bytes::from(format!("data: {}\n\n", json!({ "id": cid, "object": "chat.completion.chunk", "created": created, "model": model, "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] }))))).is_ok() } };
                chunk(&tx, json!({ "role": "assistant", "content": "" }), Value::Null);
                let h = hub.clone();
                tokio::spawn(async move {
                    let (ttx, mut trx) = mpsc::unbounded_channel::<String>();
                    let cancel = run.cancel.clone();
                    let fwd = { let (tx, cancel, chunk) = (tx.clone(), cancel.clone(), chunk.clone()); tokio::spawn(async move { while let Some(t) = trx.recv().await { if !chunk(&tx, json!({ "content": t }), Value::Null) { cancel.fire(); } } }) };
                    let res = complete(&h, &user, &pid, &model, &system, &msgs, Some(ttx), cancel, max).await;
                    let _ = fwd.await;
                    match res {
                        Ok(an) => {
                            chunk(&tx, json!({}), json!("stop"));
                            if an.usage.is_object() { let _ = tx.send(Ok(Bytes::from(format!("data: {}\n\n", json!({ "id": cid, "object": "chat.completion.chunk", "created": created, "model": if an.model.is_empty() { model.clone() } else { an.model.clone() }, "choices": [], "usage": total(&an.usage) }))))); }
                            let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n")));
                        }
                        Err(e) => { let _ = tx.send(Ok(Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", json!({ "error": { "message": e.msg, "type": "api_error" } }))))); }
                    }
                    drop(run);
                });
                return sse_answer(rx);
            }
            let res = complete(hub, &user, &pid, &model, &system, &msgs, None, run.cancel.clone(), max).await;
            drop(run);
            match res {
                Ok(an) => jout(200, json!({ "id": cid, "object": "chat.completion", "created": created, "model": if an.model.is_empty() { model } else { an.model }, "choices": [{ "index": 0, "message": { "role": "assistant", "content": an.text }, "finish_reason": "stop" }], "usage": if an.usage.is_object() { total(&an.usage) } else { json!({ "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }) } })),
                Err(e) => oa_error(if e.status == 0 { 502 } else { e.status }, &e.msg, if e.status == 503 { "service_unavailable" } else { "api_error" }),
            }
        }
        _ => fail(404, "Unknown endpoint. This hub serves GET /v1/models, POST /v1/chat/completions (OpenAI) and POST /v1/messages (Anthropic).", "not_found_error"),
    }
}

/// Settings → API access and → AI providers (signed in as `a`).
pub fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    let api = &hub.mods.api; let u = &a.username; let w = &hub.mods.work;
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let b = r.json();
    let list = || json!(super::work::providers_list(hub, u));
    Some(match r.path.as_str() {
        "/api/apikey" if get => answer(200, api.status(u)),
        "/api/apikey/rotate" if post => {
            let key = format!("mills_{}", vr_core::crypto::random_hex(24));
            let mut k = api.key_of(u); if !k.is_object() { k = json!({}); }
            let days = if b.get("expiresDays").map(|v| !v.is_null()).unwrap_or(false) { clamp_days(&b["expiresDays"]) } else { clamp_days(&k["expiresDays"]) };
            k["hash"] = json!(vr_core::crypto::sha256_hex(key.as_bytes())); k["prefix"] = json!(&key[..12]); k["createdAt"] = json!(super::auth::iso_now()); k["expiresDays"] = json!(days); k["calls"] = json!(0);
            api.set_key(u, Some(k));
            let mut st = api.status(u); st["key"] = json!(key);
            let mut o = Map::new(); o.insert("key".into(), json!(key)); for (k2, v) in st.as_object().cloned().unwrap_or_default() { if k2 != "key" { o.insert(k2, v); } }
            answer(200, Value::Object(o))
        }
        "/api/apikey/settings" if post => {
            let mut k = api.key_of(u); if !k.is_object() { k = json!({}); }
            if b.get("expiresDays").is_some() { k["expiresDays"] = json!(clamp_days(&b["expiresDays"])); }
            if b.get("provider").is_some() {
                let want = match &b["provider"] { Value::String(x) => x.clone(), v => v.to_string() };
                let Some(p) = w.provider_get(&hub.mods.ai, &want, u) else { return Some(answer(400, json!({ "error": "Unknown provider." }))) };
                k["provider"] = p["id"].clone();
                if !p["models"].as_array().map(|m| m.contains(&k["model"])).unwrap_or(false) { k["model"] = json!(""); }
            }
            if b.get("model").is_some() {
                let p = w.effective(&hub.mods.ai, u, &s(&k, "provider"));
                k["model"] = if p.map(|p| p["models"].as_array().map(|m| m.contains(&b["model"])).unwrap_or(false)).unwrap_or(false) { b["model"].clone() } else { json!("") };
            }
            api.set_key(u, Some(k));
            answer(200, api.status(u))
        }
        "/api/apikey/revoke" if post => { api.set_key(u, None); answer(200, api.status(u)) }
        "/api/providers" if get => { let p = s(&w.state(a), "provider"); answer(200, json!({ "providers": list(), "provider": if p.is_empty() { "host".to_string() } else { p } })) }
        "/api/providers/add" if post => match add_provider(hub, u, &b) { Ok(p) => answer(200, json!({ "provider": p, "providers": list() })), Err(e) => answer(400, json!({ "error": e })) },
        "/api/providers/update" if post => match update_provider(hub, u, &s(&b, "id"), &b) { Ok(p) => answer(200, json!({ "provider": p, "providers": list() })), Err(e) => answer(400, json!({ "error": e })) },
        "/api/providers/remove" if post => match remove_provider(hub, u, &s(&b, "id")) { Ok(()) => answer(200, json!({ "providers": list() })), Err(e) => answer(400, json!({ "error": e })) },
        // Settings → My AI hosts' list (+ the default provider, which this module knows)
        "/api/hosts" if get => {
            let ai = &hub.mods.ai;
            let all: Vec<Value> = ai.all().into_iter().map(|h| { let owner = h["user"] == json!(u.as_str()); json!({ "id": h["id"], "nickname": h["nickname"], "user": h["user"], "online": h["online"], "providers": h["providers"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| owner || x["allowedUsers"].as_array().map(|l| l.contains(&json!(u.as_str()))).unwrap_or(false)).map(|x| json!({ "id": x["id"], "kind": x["kind"], "label": x["label"], "models": x["models"], "capped": x["capped"] })).collect::<Vec<_>>() }) }).collect();
            answer(200, json!({ "mine": ai.mine(u), "all": all, "defaultProvider": w.default_for(ai, u).map(|p| s(&p, "id")).unwrap_or_default(), "group": hub.cfg.get("hostGroup").cloned().unwrap_or(json!([])) }))
        }
        _ => return None,
    })
}
