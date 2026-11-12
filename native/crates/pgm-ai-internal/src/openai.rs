//! The external API (moved from bridge/openai.js and bridge/providers.js): OpenAI- and Anthropic-compatible, one key
//! per person, both formats on every provider, and the providers themselves (Settings → AI providers).
//!   GET  /v1/models                  the models of the key's provider (Anthropic's list shape with anthropic-version)
//!   POST /v1/chat/completions        OpenAI chat format in (stream or not)
//!   POST /v1/messages                Anthropic Messages format in (stream or not: Anthropic's SSE events)
//!   POST /v1/messages/count_tokens   a rough count (about 4 characters a token)
//!   POST /v1/responses               OpenAI's Responses API (Codex): raw where the provider speaks it, else translated (gateway.rs)
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

use crate::hub::{answer, AiHub, Resp};
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const NO_KEY_USE: &str = "This key has no provider or model it may use: its limits leave nothing of what you may use for the API.";
const NO_AI: &str ="No AI is available: this hub has no Claude of its own and no AI host offering one is connected.";

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
/// `usage` = the OpenAI-style numbers callers see (unchanged); `native` = the provider's own usage object with its cache
/// fields (Anthropic cache_read / cache_creation, OpenAI cached_tokens), which the usage ledger normalises.
pub struct Answer { text: String, model: String, usage: Value, native: Value }

/// Which key made a request and what it may do: the person's default key (the legacy fields of their record, `id`
/// "default") or one of the named keys in `keys` (add-only: the frozen JS hub ignores them).
#[derive(Clone, Debug, Default)]
pub struct KeyCtx { pub id: String, pub label: String, pub providers: Vec<String>, pub models: Vec<String>, pub max: Option<u64>, pub named: bool }
impl KeyCtx { pub fn default_key() -> KeyCtx { KeyCtx { id: "default".into(), label: "default".into(), ..Default::default() } } }
/// The answers being made: slot → (person, key id, its cancel).
type RunMap = HashMap<u64, (String, String, Arc<Cancel>)>;
pub struct Api { root: PathBuf, mills: PathBuf, keys: Mutex<Map<String, Value>>, running: Mutex<RunMap>, run_seq: std::sync::atomic::AtomicU64, http: reqwest::Client, roll: Mutex<()> }
use pgm_core_internal::json::{read_json, s, write_json_private as write_json};
fn js_len(t: &str) -> usize { t.encode_utf16().count() }

impl Api {
    pub fn new(root: &Path, accounts: &[Account]) -> Api {
        let f = root.join("api-keys.json");
        let keys = match read_json(&f).and_then(|v| v.as_object().cloned()) {
            Some(m) => m,
            None => { let mut m = Map::new(); for a in accounts { if let Some(api) = read_json(&a.dir.join("state.json")).map(|st| st["api"].clone()).filter(Value::is_object) { m.insert(a.username.clone(), api); } } write_json(&f, &Value::Object(m.clone())); m }
        };
        // (no redirects: an endpoint someone typed in must not lead this hub on to an address of its own choosing)
        let _ = pgm_net_internal::client::http_client(std::time::Duration::from_secs(15));
        let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15)).redirect(reqwest::redirect::Policy::none()).build().expect("http");
        Api { root: root.to_path_buf(), mills: root.join(".."), keys: Mutex::new(keys), running: Mutex::default(), run_seq: Default::default(), http, roll: Mutex::default() }
    }
    fn save(&self) { write_json(&self.root.join("api-keys.json"), &Value::Object(self.keys.lock().unwrap_or_else(|e| e.into_inner()).clone())); }
    /// A named key by id.
    fn named_key(&self, u: &str, id: &str) -> Option<Value> { self.key_of(u)["keys"].as_array()?.iter().find(|k| k["id"] == json!(id)).cloned() }
    fn key_of(&self, u: &str) -> Value { self.keys.lock().unwrap_or_else(|e| e.into_inner()).get(u).cloned().unwrap_or(json!({})) }
    fn set_key(&self, u: &str, v: Option<Value>) { { let mut k = self.keys.lock().unwrap_or_else(|e| e.into_inner()); match v { Some(v) => { k.insert(u.into(), v); } None => { k.remove(u); } } } self.save(); }
    pub fn busy(&self) -> bool { !self.running.lock().unwrap_or_else(|e| e.into_inner()).is_empty() }
    /// Cancels every API answer of that person, whichever key it came in on.
    pub fn stop(&self, u: &str) { for (user, _, c) in self.running.lock().unwrap_or_else(|e| e.into_inner()).values() { if user == u { c.fire(); } } }
    pub fn drop_provider(&self, id: &str) {
        let changed = { let mut k = self.keys.lock().unwrap_or_else(|e| e.into_inner()); let mut ch = false; for v in k.values_mut() { if v["provider"] == json!(id) { if let Some(o) = v.as_object_mut() { o.remove("provider"); ch = true; } } } ch };
        if changed { self.save(); }
    }
    fn status(&self, u: &str) -> Value {
        let a = self.key_of(u);
        let (days, expires, expired) = expiry(&a);
        let tunnel = read_json(&self.root.join("tunnel.json")).map(|t| s(&t, "url")).unwrap_or_default();
        let mut keys = vec![];
        if !s(&a, "hash").is_empty() { keys.push(key_view(&a, "default", "default")); }
        for k in a["keys"].as_array().cloned().unwrap_or_default() { keys.push(key_view(&k, &s(&k, "id"), &s(&k, "label"))); }
        json!({ "hasKey": !s(&a, "hash").is_empty(), "prefix": s(&a, "prefix"), "createdAt": a.get("createdAt").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null), "expiresDays": days, "expiresAt": expires, "expired": expired,
            "provider": if s(&a, "provider").is_empty() { "host".into() } else { s(&a, "provider") }, "model": s(&a, "model"), "endpoint": format!("{tunnel}/v1"), "calls": a["calls"].as_u64().unwrap_or(0), "lastUsed": a.get("lastUsed").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null), "keys": keys })
    }
    /// Changes the person's record (the default key's legacy fields and `keys`) and saves it; an empty record goes away.
    fn edit(&self, u: &str, f: impl FnOnce(&mut Value)) { let mut rec = self.key_of(u); if !rec.is_object() { rec = json!({}); } f(&mut rec); let empty = rec.as_object().map(|m| m.is_empty()).unwrap_or(true); self.set_key(u, if empty { None } else { Some(rec) }); }
    /// The key a request carries: its person and what that key may do. A key switched off or expired is refused.
    fn account_for_key(&self, r: &Req) -> Result<(String, KeyCtx), String> {
        let k = r.header("authorization").and_then(|v| v.strip_prefix("Bearer ")).map(|v| v.trim().to_string()).unwrap_or_else(|| r.header("x-api-key").unwrap_or("").trim().to_string());
        if !(k.len() == 54 && k.starts_with("mills_") && k[6..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())) { return Err("Missing or malformed API key. Use \"Authorization: Bearer mills_…\" (or \"x-api-key: mills_…\") with the key from Settings → API access.".into()); }
        let h = pgm_core_internal::crypto::sha256_hex(k.as_bytes());
        let found = { let all = self.keys.lock().unwrap_or_else(|e| e.into_inner());
            all.iter().find_map(|(u, v)| {
                if crate::work::same_secret(v["hash"].as_str().unwrap_or(""), &h) { return Some((u.clone(), v.clone(), KeyCtx::default_key())); }
                v["keys"].as_array()?.iter().find(|x| crate::work::same_secret(x["hash"].as_str().unwrap_or(""), &h)).map(|x| (u.clone(), x.clone(), KeyCtx {
                    id: s(x, "id"), label: s(x, "label"), named: true, max: x["maxConcurrent"].as_u64().filter(|n| *n > 0).map(|n| n.min(64)),
                    providers: str_list(&x["providers"]), models: str_list(&x["models"]) }))
            }) };
        let Some((u, rec, ctx)) = found else { return Err("Unknown API key (rotated, revoked, or never issued).".into()) };
        if ctx.named && rec["enabled"] == json!(false) { return Err("This API key is switched off.".into()); }
        if expiry(&rec).2 { return Err("This API key has expired; rotate it (or make a new one) in Settings → API access.".into()); }
        Ok((u, ctx))
    }
}
fn str_list(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }
/// (expiresDays, expiry in ms, expired) of a key record.
fn expiry(a: &Value) -> (u64, Option<u64>, bool) {
    let days = a["expiresDays"].as_u64().unwrap_or(0);
    let created = chrono::DateTime::parse_from_rfc3339(a["createdAt"].as_str().unwrap_or("")).ok().map(|d| d.timestamp_millis() as u64);
    let expires = match (created, days) { (Some(c), d) if d > 0 => Some(c + d * 86_400_000), _ => None };
    (days, expires, expires.map(|e| e < now_ms()).unwrap_or(false))
}
/// A key as the list shows it: never the secret or its hash.
fn key_view(k: &Value, id: &str, label: &str) -> Value {
    let (days, expires, expired) = expiry(k);
    json!({ "id": id, "label": label, "prefix": s(k, "prefix"), "createdAt": k.get("createdAt").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null), "expiresDays": days, "expiresAt": expires, "expired": expired,
        "enabled": k["enabled"] != json!(false), "calls": k["calls"].as_u64().unwrap_or(0), "lastUsed": k.get("lastUsed").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null),
        "providers": str_list(&k["providers"]), "models": str_list(&k["models"]), "maxConcurrent": k.get("maxConcurrent").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null), "default": id == "default" })
}
fn clamp_days(d: &Value) -> u64 { let n = d.as_f64().or_else(|| d.as_str().and_then(|x| x.parse().ok())).unwrap_or(0.0).round() as i64; if [0, 1, 7, 30, 90].contains(&n) { n as u64 } else { 0 } }

// ---------- providers (Settings → AI providers): bridge/providers.json ----------
fn providers_file(hub: &dyn AiHub) -> PathBuf { hub.root().join("providers.json") }
fn providers_db(hub: &dyn AiHub) -> Vec<Value> { read_json(&providers_file(hub)).and_then(|v| v["providers"].as_array().cloned()).unwrap_or_default() }
fn save_providers(hub: &dyn AiHub, list: Vec<Value>) { write_json(&providers_file(hub), &json!({ "providers": list })); }
fn host_models(hub: &dyn AiHub) -> Vec<String> { hub.cfg().get("models").and_then(Value::as_array).map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default() }
fn clean_models(m: &Value, fallback: Vec<String>) -> Vec<String> {
    let list: Vec<String> = match m { Value::Array(a) => a.iter().map(|x| match x { Value::String(t) => t.trim().to_string(), Value::Null => String::new(), v => v.to_string() }).collect(), Value::String(t) => t.split(|c: char| c == ',' || c.is_whitespace()).map(|x| x.trim().to_string()).collect(), Value::Null => vec![], v => vec![v.to_string()] };
    let list: Vec<String> = list.into_iter().filter(|x| !x.is_empty()).take(20).collect();
    if list.is_empty() { fallback } else { list }
}
fn pub_view(p: &Value) -> Value {
    let mut v = json!({ "id": p["id"], "kind": p["kind"], "label": p["label"], "owner": p["owner"], "shared": p["shared"] == json!(true), "models": if p["models"].is_array() { p["models"].clone() } else { json!([]) }, "hasKey": !s(p, "apiKey").is_empty(), "createdAt": p["createdAt"] });
    for k in ["baseUrl", "configDir", "command", "home", "provider"] { if !s(p, k).is_empty() { v[k] = p[k].clone(); } }
    if p["args"].is_array() { v["args"] = p["args"].clone(); }
    if p["responses"] == json!(true) { v["responses"] = json!(true); }
    if p["kind"] == json!(crate::jcode::KIND) { v["harness"] = json!(crate::jcode::HARNESS); }
    v
}
/// A custom endpoint (this hub calls whatever address it names), a Claude config folder or a Jcode PGM binary on this
/// machine: admins only.
pub fn admin_only(p: &Value) -> bool { matches!(p["kind"].as_str(), Some("openai") | Some("claude-cli") | Some("jcode")) }
fn add_provider(hub: &dyn AiHub, user: &str, b: &Value) -> Result<Value, String> {
    if admin_only(b) {
        // a Jcode PGM binary runs here on the hub, so only being an admin counts; the other two need an AI host of theirs
        let needs_host = s(b, "kind") != crate::jcode::KIND;
        if !hub.is_admin(user) || (needs_host && !hub.ai().owns_online_host(user)) { return Err("A custom endpoint or a Claude config folder is for admins signed in on an AI host of their own (Settings → My AI hosts); a Jcode PGM binary is for admins. An Anthropic API key works for everyone.".into()); }
    }
    let kind = s(b, "kind");
    if !["anthropic", "claude-cli", "openai", "jcode"].contains(&kind.as_str()) { return Err("kind must be anthropic, claude-cli, openai or jcode.".into()); }
    let label: String = s(b, "label").trim().chars().take(60).collect();
    if label.is_empty() { return Err("Give it a label.".into()); }
    let mut p = json!({ "id": format!("p-{}", pgm_core_internal::crypto::random_hex(5)), "kind": kind, "label": label, "owner": user, "shared": b["shared"].as_bool().unwrap_or(false), "createdAt": pgm_core_internal::time::iso_now(), "models": [] });
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
        "jcode" => {
            // a Jcode PGM binary on this machine (its path, or just its name on PATH), optional extra args, an optional
            // jcode home (else each person gets <profile>/jcode-home), the jcode provider profile to run with (-p), and
            // the models it may be asked for (-m; empty = jcode's own default)
            let c = s(b, "command").trim().to_string();
            let c = if c.is_empty() { crate::jcode::DEFAULT_COMMAND.to_string() } else { c };
            if c.contains('\n') || c.contains('\0') { return Err("command must be a binary path or name.".into()); }
            p["command"] = json!(c);
            if let Some(a) = b["args"].as_array() { let a: Vec<String> = a.iter().filter_map(|x| x.as_str().map(|t| t.trim().to_string())).filter(|t| !t.is_empty()).collect(); if !a.is_empty() { p["args"] = json!(a); } }
            let h = s(b, "home").trim().to_string();
            if !h.is_empty() { if !Path::new(&h).is_absolute() { return Err("home must be an absolute folder on the host (the jcode home, JCODE_HOME, Work runs use).".into()); } p["home"] = json!(h); }
            let pr: String = s(b, "provider").trim().chars().take(80).collect();
            if !pr.is_empty() { p["provider"] = json!(pr); }
            p["models"] = json!(clean_models(&b["models"], vec![]));
        }
        _ => {
            let url = s(b, "baseUrl").trim().trim_end_matches('/').to_string();
            let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or("");
            if rest.is_empty() || rest.starts_with('/') || rest.chars().next().map(char::is_whitespace).unwrap_or(true) { return Err("baseUrl must be an http(s) address, e.g. https://api.example.com/v1".into()); }
            p["baseUrl"] = json!(url); p["apiKey"] = json!(s(b, "apiKey").trim());
            let m = clean_models(&b["models"], vec![]);
            if m.is_empty() { return Err("List at least one model id the endpoint serves.".into()); }
            p["models"] = json!(m);
            // G5: the endpoint speaks OpenAI's Responses API itself (POST <baseUrl>/responses), so /v1/responses goes to it raw;
            // api.openai.com counts as such without the flag
            if b["responses"] == json!(true) { p["responses"] = json!(true); }
        }
    }
    let mut list = providers_db(hub); list.push(p.clone()); save_providers(hub, list);
    Ok(pub_view(&p))
}
fn update_provider(hub: &dyn AiHub, user: &str, id: &str, b: &Value) -> Result<Value, String> {
    let mut list = providers_db(hub);
    let Some(p) = list.iter_mut().find(|x| x["id"] == json!(id)).filter(|x| x["owner"] == json!(user)) else { return Err("Not your provider.".into()) };
    if !b["label"].is_null() { let l: String = match &b["label"] { Value::String(t) => t.trim().chars().take(60).collect(), v => v.to_string() }; if !l.is_empty() { p["label"] = json!(l); } }
    if b.get("shared").is_some() { p["shared"] = json!(!(b["shared"].is_null() || b["shared"] == json!(false) || b["shared"] == json!(0) || b["shared"] == json!(""))); }
    if b.get("models").is_some() { let fb = if p["kind"] == json!("openai") || p["kind"] == json!("jcode") { p["models"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() } else { host_models(hub) }; p["models"] = json!(clean_models(&b["models"], fb)); }
    if !s(b, "apiKey").is_empty() && p["kind"] != json!("claude-cli") && p["kind"] != json!("jcode") { p["apiKey"] = json!(s(b, "apiKey").trim()); }
    if p["kind"] == json!("openai") && b.get("responses").is_some() { if b["responses"] == json!(true) { p["responses"] = json!(true); } else if let Some(m) = p.as_object_mut() { m.remove("responses"); } }
    if p["kind"] == json!("jcode") {
        if !s(b, "command").trim().is_empty() { p["command"] = json!(s(b, "command").trim()); }
        if b.get("provider").is_some() { let pr: String = s(b, "provider").trim().chars().take(80).collect(); if pr.is_empty() { p.as_object_mut().map(|m| m.remove("provider")); } else { p["provider"] = json!(pr); } }
        if let Some(h) = b.get("home").and_then(Value::as_str) { let h = h.trim(); if h.is_empty() { p.as_object_mut().map(|m| m.remove("home")); } else if Path::new(h).is_absolute() { p["home"] = json!(h); } }
    }
    let v = pub_view(p); save_providers(hub, list);
    Ok(v)
}
fn remove_provider(hub: &dyn AiHub, user: &str, id: &str) -> Result<(), String> {
    let mut list = providers_db(hub);
    let Some(i) = list.iter().position(|x| x["id"] == json!(id)).filter(|i| list[*i]["owner"] == json!(user)) else { return Err("Not your provider.".into()) };
    list.remove(i); save_providers(hub, list);
    // nobody keeps pointing at it
    for a in hub.accounts() { hub.work().drop_provider(&a, id); }
    hub.api().drop_provider(id);
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

/// The provider's own usage object, in whatever shape its kind gives, as the ledger's one shape (input paid, output,
/// cache read, cache written). None when the answer carried no usage.
pub fn normalise(native: &Value) -> Option<pgm_context_internal::ledger::Usage> { pgm_context_internal::ledger::parse_usage(native) }
/// The Responses API (Codex) counts `input_tokens` with the cached ones included: as OpenAI chat's shape for the ledger.
fn responses_native(u: &Value) -> Value {
    json!({ "prompt_tokens": u["input_tokens"].as_u64().unwrap_or(0), "completion_tokens": u["output_tokens"].as_u64().unwrap_or(0), "prompt_tokens_details": { "cached_tokens": u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0) } })
}
/// One answer for a person, through whichever provider they use; what it used goes to the usage ledger (`answers` in
/// /api/usage: per person and provider kind, with the cache hit ratio).
async fn complete(hub: &Arc<dyn AiHub>, user: &str, provider: &str, model: &str, system: &str, messages: &[(String, String)], on_token: Tok, cancel: Arc<Cancel>, max_tokens: Option<u64>) -> Result<Answer, ApiErr> {
    let an = complete_inner(hub, user, provider, model, system, messages, on_token, cancel, max_tokens).await?;
    if let Some(u) = normalise(&an.native) {
        let (w, ai) = (hub.work(), hub.ai());
        let kind = (if provider.is_empty() { None } else { w.provider_get(ai, provider, user) }).or_else(|| w.default_for(ai, user)).map(|p| s(&p, "kind")).unwrap_or_default();
        ai.usage.note_answer(user, &kind, &an.model, &u);
    }
    Ok(an)
}
/// One answer for a person outside Work and the API (a Hyper Workflows node, T-204): through their provider (`provider`
/// empty: their default), counted in the usage ledger like every other answer. `(text, model, usage)`, usage in the
/// OpenAI shape (`prompt_tokens`, `completion_tokens`).
#[allow(clippy::too_many_arguments)]
pub async fn answer_for(hub: &Arc<dyn AiHub>, user: &str, provider: &str, model: &str, system: &str, messages: &[(String, String)], cancel: Arc<Cancel>, max_tokens: Option<u64>) -> Result<(String, String, Value), ApiErr> {
    let a = complete(hub, user, provider, model, system, messages, None, cancel, max_tokens).await?;
    Ok((a.text, a.model, a.usage))
}
async fn complete_inner(hub: &Arc<dyn AiHub>, user: &str, provider: &str, model: &str, system: &str, messages: &[(String, String)], on_token: Tok, cancel: Arc<Cancel>, max_tokens: Option<u64>) -> Result<Answer, ApiErr> {
    let w = hub.work(); let ai = hub.ai();
    let p = (if provider.is_empty() { None } else { w.provider_get(ai, provider, user) }).or_else(|| w.default_for(ai, user)).ok_or_else(|| err(503, ai.with_peer_note(NO_AI)))?;
    let kind = s(&p, "kind");
    if kind == "host-jcode" { return Err(err(400, "That AI is a Jcode PGM host: it runs Work, not API answers. Pick a Claude, API or local-model provider for answers.")); }
    if (kind == "host" || kind == "claude-cli") && hub.root().join("ai-host.off").exists() { return Err(err(503, "The host's AI is switched off right now (its host paused it); an API key or endpoint of your own still answers.")); }
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
                let mut rb = self_http(&**hub).post(format!("{}/chat/completions", s(&p, "baseUrl"))).header("content-type", "application/json").body(body.to_string());
                if !s(&p, "apiKey").is_empty() { rb = rb.header("authorization", format!("Bearer {}", s(&p, "apiKey"))); }
                let r = rb.send().await.map_err(|e| err(502, e.to_string()))?;
                (r.status().as_u16(), r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string(), Src::Http(r))
            } else {
                let r = ai.provider_fetch_for(user, "", &host, &pid, "/chat/completions", "POST", vec![("content-type".into(), "application/json".into())], body.to_string().into_bytes()).await.map_err(|e| err(e.status, e.msg))?;
                (r.status, r.headers.iter().find(|(k, _)| k == "content-type").map(|x| x.1.clone()).unwrap_or_default(), Src::Host(r.body))
            };
            let who = if kind == "openai" { "The endpoint".to_string() } else { format!("{nick}'s endpoint") };
            if !(200..300).contains(&status) { return Err(fail_text(who, status, src.all().await, false)); }
            let used = s(&body, "model");
            if !streaming || (kind == "host-openai" && !ct.to_lowercase().contains("text/event-stream")) {
                let j: Value = serde_json::from_slice(&src.all().await).unwrap_or(Value::Null);
                let text = j["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
                if streaming && !text.is_empty() { tok(&on_token, &text); }
                return Ok(Answer { text, model: j["model"].as_str().map(String::from).unwrap_or(used), usage: j["usage"].clone(), native: j["usage"].clone() });
            }
            let (mut text, mut used, mut usage, mut sse, mut src) = (String::new(), used, Value::Null, Sse::default(), src);
            while let Some(c) = next_or_cancel(&mut src, &cancel).await? { for ev in sse.push(&c) {
                if let Some(d) = ev["choices"][0]["delta"]["content"].as_str() { text.push_str(d); tok(&on_token, d); }
                if let Some(x) = ev["model"].as_str() { used = x.into(); } if !ev["usage"].is_null() { usage = ev["usage"].clone(); }
            } if sse.done { break; } }
            Ok(Answer { text, model: used, native: usage.clone(), usage })
        }
        "anthropic" | "host-claude" if !(kind == "host-claude" && s(&p, "remoteKind") == "claude-cli-work") => {
            let mut body = json!({ "model": if m.is_empty() { p["models"][0].clone() } else { json!(m) }, "max_tokens": max_tokens.unwrap_or(4096), "stream": streaming, "messages": ant_msgs() });
            let (status, src) = if kind == "anthropic" {
                if !system.is_empty() { body["system"] = json!(system); }
                let r = self_http(&**hub).post(ANTHROPIC_URL).header("content-type", "application/json").header("x-api-key", s(&p, "apiKey")).header("anthropic-version", "2023-06-01").body(body.to_string()).send().await.map_err(|e| err(502, e.to_string()))?;
                (r.status().as_u16(), Src::Http(r))
            } else {
                // a subscription login answers only when the system prompt opens as Claude Code
                let mut sys = vec![json!({ "type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude." })]; if !system.is_empty() { sys.push(json!({ "type": "text", "text": system })); }
                body["system"] = json!(sys);
                let r = ai.login_fetch_for(user, "", &host, &pid, "/v1/messages", "POST", vec![("content-type".into(), "application/json".into()), ("anthropic-version".into(), "2023-06-01".into())], body.to_string().into_bytes()).await.map_err(|e| err(e.status, e.msg))?;
                (r.status, Src::Host(r.body))
            };
            if !(200..300).contains(&status) { return Err(if kind == "anthropic" { err(502, format!("Anthropic answered {status}: {}", String::from_utf8_lossy(&src.all().await).chars().take(300).collect::<String>())) } else { fail_text(format!("{nick}'s Claude"), status, src.all().await, true) }); }
            if !streaming {
                let j: Value = serde_json::from_slice(&src.all().await).unwrap_or(Value::Null);
                let text = j["content"].as_array().map(|c| c.iter().filter_map(|b| b["text"].as_str()).collect::<String>()).unwrap_or_default();
                let usage = if j["usage"].is_object() { json!({ "prompt_tokens": j["usage"]["input_tokens"], "completion_tokens": j["usage"]["output_tokens"] }) } else { Value::Null };
                return Ok(Answer { text, model: s(&j, "model"), usage, native: j["usage"].clone() });
            }
            let (mut text, mut used, mut usage, mut sse, mut src) = (String::new(), s(&body, "model"), Value::Null, Sse::default(), src);
            let mut native = json!({}); // Anthropic's own: message_start carries the input and cache numbers, message_delta the output
            while let Some(c) = next_or_cancel(&mut src, &cancel).await? { for ev in sse.push(&c) {
                match ev["type"].as_str().unwrap_or("") {
                    "content_block_delta" => if let Some(t) = ev["delta"]["text"].as_str() { text.push_str(t); tok(&on_token, t); },
                    "message_start" => { if let Some(x) = ev["message"]["model"].as_str() { used = x.into(); } usage = json!({ "prompt_tokens": ev["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0), "completion_tokens": 0 }); if ev["message"]["usage"].is_object() { native = ev["message"]["usage"].clone(); } }
                    "message_delta" => if usage.is_object() && ev["usage"].is_object() { usage["completion_tokens"] = json!(ev["usage"]["output_tokens"].as_u64().unwrap_or(0)); native["output_tokens"] = json!(ev["usage"]["output_tokens"].as_u64().unwrap_or(0)); },
                    _ => {}
                }
            } if sse.done { break; } }
            Ok(Answer { text, model: used, usage, native })
        }
        "host-codex" => {
            let input: Vec<Value> = messages.iter().filter(|x| x.0 != "system").map(|x| { let asst = x.0 == "assistant"; json!({ "role": if asst { "assistant" } else { "user" }, "content": [{ "type": if asst { "output_text" } else { "input_text" }, "text": x.1 }] }) }).collect();
            let body = json!({ "model": if m.is_empty() { p["models"][0].clone() } else { json!(m) }, "instructions": if system.is_empty() { "You are a helpful assistant." } else { system }, "input": input, "stream": true, "store": false });
            let r = ai.login_fetch_for(user, "", &host, &pid, "/responses", "POST", vec![("content-type".into(), "application/json".into()), ("accept".into(), "text/event-stream".into())], body.to_string().into_bytes()).await.map_err(|e| err(e.status, e.msg))?;
            if !(200..300).contains(&r.status) { let st = r.status; return Err(fail_text(format!("{nick}'s Codex"), st, Src::Host(r.body).all().await, true)); }
            let (mut text, mut used, mut usage, mut sse, mut src) = (String::new(), s(&body, "model"), Value::Null, Sse::default(), Src::Host(r.body));
            let mut native = Value::Null;
            while let Some(c) = next_or_cancel(&mut src, &cancel).await? { for ev in sse.push(&c) {
                match ev["type"].as_str().unwrap_or("") {
                    "response.output_text.delta" => if let Some(d) = ev["delta"].as_str().filter(|d| !d.is_empty()) { text.push_str(d); tok(&on_token, d); },
                    "response.completed" => { if let Some(x) = ev["response"]["model"].as_str() { used = x.into(); } let u = &ev["response"]["usage"]; if u.is_object() { usage = json!({ "prompt_tokens": u["input_tokens"].as_u64().unwrap_or(0), "completion_tokens": u["output_tokens"].as_u64().unwrap_or(0) }); native = responses_native(u); } }
                    _ => {}
                }
            } if sse.done { break; } }
            Ok(Answer { text, model: used, usage, native })
        }
        "host-claude" => { // legacy: a host that allows work runs a sandboxed `claude -p` there
            let (id, _t, mut rx) = ai.run_work_as(user, &host, &pid, json!({ "sandbox": true, "account": "api", "display": "API", "message": flatten(messages), "systemPrompt": system, "model": m, "hubUrl": "" })).await.map_err(|e| err(503, e.msg))?;
            let (mut text, mut used, mut usage, mut er) = (String::new(), m.clone(), Value::Null, String::new());
            loop {
                tokio::select! {
                    _ = cancel.woke.notified() => { ai.cancel_work(&id); return Err(err(502, "cancelled")); }
                    msg = rx.recv() => match msg {
                        Some(crate::ai_hosts::WorkMsg::Event(ev)) => claude_event(&ev, &mut text, &mut used, &mut usage, &mut er, &on_token),
                        Some(crate::ai_hosts::WorkMsg::Done(_)) | None => break,
                        Some(crate::ai_hosts::WorkMsg::Error(e)) => return Err(err(503, e)),
                    }
                }
            }
            if text.is_empty() { return Err(err(502, if er.trim().is_empty() { "the host's Claude gave no answer".to_string() } else { er.trim().to_string() })); }
            Ok(Answer { text, model: used, native: usage.clone(), usage: cli_usage(&usage) })
        }
        _ => complete_cli(hub, &p, &m, system, messages, on_token, cancel).await, // this hub's Claude, or a Claude Code login here
    }
}
/// An answer that may carry tool calls (T-317), in OpenAI's shape whatever the provider speaks.
pub struct ToolAnswer { text: String, calls: Vec<Value>, finish: String, model: String, usage: Value }

async fn read_all(mut src: Src, cancel: &Cancel) -> Result<Vec<u8>, ApiErr> { let mut v = vec![]; while let Some(c) = next_or_cancel(&mut src, cancel).await? { v.extend(c); } Ok(v) }

/// One answer with tools (`tools`, `tool_choice`, assistant `tool_calls` and `tool` messages of OpenAI's chat format), as
/// [`complete`] notes its use. OpenAI-kind providers get the fields as they are; Anthropic ones (an API key, a host's Claude
/// login) get tool_use / tool_result blocks; any other kind refuses with 400 ("cannot carry tools") so a caller can fall back
/// to text. Never streamed from the provider: the answer is whole when it is sent on.
async fn complete_tools(hub: &Arc<dyn AiHub>, user: &str, provider: &str, model: &str, b: &Value, cancel: Arc<Cancel>, max_tokens: Option<u64>) -> Result<ToolAnswer, ApiErr> {
    let w = hub.work(); let ai = hub.ai();
    let p = (if provider.is_empty() { None } else { w.provider_get(ai, provider, user) }).or_else(|| w.default_for(ai, user)).ok_or_else(|| err(503, ai.with_peer_note(NO_AI)))?;
    let kind = s(&p, "kind");
    if (kind == "host" || kind == "claude-cli" || kind == "host-claude") && hub.root().join("ai-host.off").exists() { return Err(err(503, "The host's AI is switched off right now (its host paused it); an API key or endpoint of your own still answers.")); }
    let m = pick_model(&p, model);
    let host = s(&p, "hostId"); let pid = s(&p, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default(); let nick = s(&p, "nickname");
    let used_model = if m.is_empty() { p["models"][0].clone() } else { json!(m) };
    let ans = match kind.as_str() {
        "openai" | "host-openai" => {
            let mut body = json!({ "model": used_model, "stream": false, "messages": crate::toolconv::clean_messages(b) });
            if b["tools"].is_array() { body["tools"] = b["tools"].clone(); }
            if !b["tool_choice"].is_null() { body["tool_choice"] = b["tool_choice"].clone(); }
            if let Some(mt) = max_tokens { body["max_tokens"] = json!(mt); }
            let (status, src) = if kind == "openai" {
                let mut rb = self_http(&**hub).post(format!("{}/chat/completions", s(&p, "baseUrl"))).header("content-type", "application/json").body(body.to_string());
                if !s(&p, "apiKey").is_empty() { rb = rb.header("authorization", format!("Bearer {}", s(&p, "apiKey"))); }
                let r = rb.send().await.map_err(|e| err(502, e.to_string()))?;
                (r.status().as_u16(), Src::Http(r))
            } else {
                let r = ai.provider_fetch_for(user, "", &host, &pid, "/chat/completions", "POST", vec![("content-type".into(), "application/json".into())], body.to_string().into_bytes()).await.map_err(|e| err(e.status, e.msg))?;
                (r.status, Src::Host(r.body))
            };
            let raw = read_all(src, &cancel).await?;
            if !(200..300).contains(&status) { return Err(err(502, format!("{} answered {status}: {}", if kind == "openai" { "The endpoint".to_string() } else { format!("{nick}'s endpoint") }, String::from_utf8_lossy(&raw).chars().take(300).collect::<String>()))); }
            let j: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let msg = &j["choices"][0]["message"];
            let calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();
            let finish = j["choices"][0]["finish_reason"].as_str().map(String::from).unwrap_or_else(|| if calls.is_empty() { "stop".into() } else { "tool_calls".into() });
            (ToolAnswer { text: msg["content"].as_str().unwrap_or("").to_string(), calls, finish, model: j["model"].as_str().map(String::from).unwrap_or_else(|| s(&body, "model")), usage: j["usage"].clone() }, j["usage"].clone())
        }
        "anthropic" | "host-claude" if !(kind == "host-claude" && s(&p, "remoteKind") == "claude-cli-work") => {
            let (system, msgs, tools, choice) = crate::toolconv::to_anthropic(b);
            if msgs.is_empty() { return Err(err(400, "\"messages\" needs a user or assistant message.")); }
            let mut body = json!({ "model": used_model, "max_tokens": max_tokens.unwrap_or(4096), "stream": false, "messages": msgs });
            if !tools.is_empty() { body["tools"] = json!(tools); }
            if let Some(c) = choice { body["tool_choice"] = c; }
            let (status, src) = if kind == "anthropic" {
                if !system.is_empty() { body["system"] = json!(system); }
                let r = self_http(&**hub).post(ANTHROPIC_URL).header("content-type", "application/json").header("x-api-key", s(&p, "apiKey")).header("anthropic-version", "2023-06-01").body(body.to_string()).send().await.map_err(|e| err(502, e.to_string()))?;
                (r.status().as_u16(), Src::Http(r))
            } else {
                let mut sys = vec![json!({ "type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude." })]; if !system.is_empty() { sys.push(json!({ "type": "text", "text": system })); }
                body["system"] = json!(sys);
                let r = ai.login_fetch_for(user, "", &host, &pid, "/v1/messages", "POST", vec![("content-type".into(), "application/json".into()), ("anthropic-version".into(), "2023-06-01".into())], body.to_string().into_bytes()).await.map_err(|e| err(e.status, e.msg))?;
                (r.status, Src::Host(r.body))
            };
            let raw = read_all(src, &cancel).await?;
            if !(200..300).contains(&status) { return Err(err(if status == 429 && kind == "host-claude" { 429 } else { 502 }, format!("{} answered {status}: {}", if kind == "anthropic" { "Anthropic".to_string() } else { format!("{nick}'s Claude") }, String::from_utf8_lossy(&raw).chars().take(300).collect::<String>()))); }
            let j: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let (text, calls, finish) = crate::toolconv::from_anthropic_answer(&j);
            let usage = if j["usage"].is_object() { json!({ "prompt_tokens": j["usage"]["input_tokens"], "completion_tokens": j["usage"]["output_tokens"] }) } else { Value::Null };
            (ToolAnswer { text, calls, finish, model: s(&j, "model"), usage }, j["usage"].clone())
        }
        _ => return Err(err(400, "That provider cannot carry tools: tool calls go through an OpenAI-compatible endpoint or an Anthropic one (an API key or a host's Claude login). Ask without tools for a text answer.")),
    };
    if let Some(u) = normalise(&ans.1) { ai.usage.note_answer(user, &kind, &ans.0.model, &u); }
    Ok(ans.0)
}
/// The answer of a chat request with tools: whole, or (stream: true) as chat.completion.chunk events with the calls in `delta.tool_calls`.
fn tools_reply(an: ToolAnswer, model: String, cid: &str, created: u64, stream: bool) -> Resp {
    let model = if an.model.is_empty() { model } else { an.model.clone() };
    let usage = if an.usage.is_object() { total(&an.usage) } else { json!({ "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }) };
    if !stream {
        let mut msg = json!({ "role": "assistant", "content": if an.text.is_empty() && !an.calls.is_empty() { Value::Null } else { json!(an.text) } });
        if !an.calls.is_empty() { msg["tool_calls"] = json!(an.calls); }
        return jout(200, json!({ "id": cid, "object": "chat.completion", "created": created, "model": model, "choices": [{ "index": 0, "message": msg, "finish_reason": an.finish }], "usage": usage }));
    }
    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, String>>();
    let chunk = |delta: Value, finish: Value| { let _ = tx.send(Ok(Bytes::from(format!("data: {}\n\n", json!({ "id": cid, "object": "chat.completion.chunk", "created": created, "model": model, "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] }))))); };
    chunk(json!({ "role": "assistant", "content": "" }), Value::Null);
    if !an.text.is_empty() { chunk(json!({ "content": an.text }), Value::Null); }
    for (i, c) in an.calls.iter().enumerate() { chunk(json!({ "tool_calls": [{ "index": i, "id": c["id"], "type": "function", "function": c["function"] }] }), Value::Null); }
    chunk(json!({}), json!(an.finish));
    let _ = tx.send(Ok(Bytes::from(format!("data: {}\n\n", json!({ "id": cid, "object": "chat.completion.chunk", "created": created, "model": model, "choices": [], "usage": usage })))));
    let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n")));
    sse_answer(rx)
}
pub(crate) fn self_http(hub: &dyn AiHub) -> &reqwest::Client { &hub.api().http }
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
async fn complete_cli(hub: &Arc<dyn AiHub>, p: &Value, model: &str, system: &str, messages: &[(String, String)], on_token: Tok, cancel: Arc<Cancel>) -> Result<Answer, ApiErr> {
    let sandbox = hub.api().mills.join("Api").join("sandbox");
    let _ = std::fs::create_dir_all(&sandbox);
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose", "--permission-mode", "default", "--strict-mcp-config"].iter().map(|x| x.to_string()).collect();
    if !system.is_empty() { args.push("--append-system-prompt".into()); args.push(system.into()); }
    if !model.is_empty() { args.push("--model".into()); args.push(model.into()); }
    let command = hub.cfg().get("claudeCommand").and_then(Value::as_str).unwrap_or("claude").to_string();
    let pre: Vec<String> = hub.cfg().get("claudeArgs").and_then(Value::as_array).map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut cmd = tokio::process::Command::new(&command);
    cmd.args(&pre).args(&args).current_dir(&sandbox).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    match s(p, "kind").as_str() { "anthropic" => { cmd.env("ANTHROPIC_API_KEY", s(p, "apiKey")); } "claude-cli" => { cmd.env("CLAUDE_CONFIG_DIR", s(p, "configDir")); } _ => {} }
    #[cfg(windows)]
    { cmd.creation_flags(0x0800_0000); }
    let mut child = cmd.spawn().map_err(|e| err(502, format!("Could not start claude: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() { let inp = flatten(messages); tokio::spawn(async move { let _ = stdin.write_all(inp.as_bytes()).await; let _ = stdin.shutdown().await; }); }
    let er = Arc::new(Mutex::new(String::new()));
    let et = child.stderr.take().map(|e| { let er = er.clone(); tokio::spawn(async move { use tokio::io::AsyncReadExt; let mut e = e; let mut b = vec![0u8; 4096]; while let Ok(n) = e.read(&mut b).await { if n == 0 { break; } let mut x = er.lock().unwrap_or_else(|e| e.into_inner()); x.push_str(&String::from_utf8_lossy(&b[..n])); let k = x.chars().count(); if k > 600 { *x = x.chars().skip(k - 600).collect(); } } }) });
    let (mut text, mut used, mut usage, mut rerr) = (String::new(), model.to_string(), Value::Null, String::new());
    let mut killed = false;
    if let Some(out) = child.stdout.take() {
        let mut lines = tokio::io::BufReader::new(out).lines();
        loop {
            tokio::select! {
                _ = cancel.woke.notified(), if !killed => { killed = true; if let Some(pid) = child.id() { crate::work::kill_pid(pid); } }
                l = lines.next_line() => match l { Ok(Some(l)) => { if let Ok(ev) = serde_json::from_str::<Value>(l.trim()) { claude_event(&ev, &mut text, &mut used, &mut usage, &mut rerr, &on_token); } } _ => break },
            }
        }
    }
    let code = child.wait().await.ok().and_then(|s| s.code());
    if let Some(t) = et { let _ = t.await; }
    if killed || cancel.is() { return Err(err(502, "cancelled")); }
    let e2 = format!("{rerr}{}", er.lock().unwrap_or_else(|e| e.into_inner()));
    if text.is_empty() && (code.map(|c| c != 0).unwrap_or(false) || !e2.is_empty()) { return Err(err(502, if e2.trim().is_empty() { format!("claude exited with code {}", code.map(|c| c.to_string()).unwrap_or_else(|| "null".into())) } else { e2.trim().to_string() })); }
    Ok(Answer { text, model: used, native: usage.clone(), usage: cli_usage(&usage) })
}

// ---------- the API's answers ----------
pub(crate) fn cors(mut r: hyper::Response<pgm_net_internal::http::Body>) -> Resp {
    let h = r.headers_mut();
    h.insert("access-control-allow-origin", hyper::header::HeaderValue::from_static("*"));
    h.insert("access-control-allow-headers", hyper::header::HeaderValue::from_static("Content-Type, Authorization, x-api-key, anthropic-version, anthropic-beta"));
    r
}
fn jout(status: u16, v: Value) -> Resp { cors(pgm_net_internal::http::json(status, &v)) }
pub(crate) fn oa_error(status: u16, m: &str, t: &str) -> Resp { jout(status, json!({ "error": { "message": m, "type": t, "code": Value::Null } })) }
pub(crate) fn ant_error(status: u16, m: &str, t: &str) -> Resp {
    let t = match t { "service_unavailable" => "overloaded_error", "rate_limit_error" | "authentication_error" | "invalid_request_error" | "not_found_error" => t, _ => "api_error" };
    jout(status, json!({ "type": "error", "error": { "type": t, "message": m } }))
}
fn sse_answer(rx: mpsc::UnboundedReceiver<Result<Bytes, String>>) -> Resp {
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
pub(crate) struct Running { hub: Arc<dyn AiHub>, slot: u64, pub(crate) cancel: Arc<Cancel> }
impl Drop for Running { fn drop(&mut self) { self.cancel.fire(); self.hub.api().running.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.slot); self.hub.work().busy_changed.notify_one(); } }
/// How many answers one key may have going at once: its own `maxConcurrent`, else config `api.maxConcurrent`, else 8.
pub(crate) fn max_concurrent(hub: &dyn AiHub, key: &KeyCtx) -> usize { key.max.or_else(|| hub.cfg().get("api").and_then(|a| a["maxConcurrent"].as_u64()).filter(|n| *n > 0)).unwrap_or(8).min(64) as usize }
/// A slot for one more answer of that key (None: it is at its limit).
pub(crate) fn start_running(hub: &Arc<dyn AiHub>, user: &str, key: &KeyCtx) -> Option<Running> {
    let max = max_concurrent(&**hub, key);
    let api = hub.api();
    let mut r = api.running.lock().unwrap_or_else(|e| e.into_inner());
    if r.values().filter(|(u, k, _)| u == user && *k == key.id).count() >= max { return None; }
    let c = Arc::new(Cancel::default()); let slot = api.run_seq.fetch_add(1, Ordering::SeqCst);
    r.insert(slot, (user.into(), key.id.clone(), c.clone())); drop(r);
    hub.work().busy_changed.notify_one();
    Some(Running { hub: hub.clone(), slot, cancel: c })
}
/// 429 with `retry-after`, in the shape the caller speaks.
pub(crate) fn too_many(anthropic: bool, key: &KeyCtx, max: usize) -> Resp {
    let m = format!("Too many requests at once on this key (the limit is {max}{}); wait for one to finish and try again.", if key.named { format!(" for key \"{}\"", key.label) } else { String::new() });
    let mut r = if anthropic { ant_error(429, &m, "rate_limit_error") } else { oa_error(429, &m, "rate_limit_error") };
    r.headers_mut().insert("retry-after", hyper::header::HeaderValue::from_static("1"));
    r
}
/// Counts a call on its key (and in the key's day roll-up, under the provider and model it was routed to); the first call
/// of a day goes to the security log (key id and prefix, never the secret).
pub(crate) fn count_use(hub: &dyn AiHub, u: &str, key: &KeyCtx, provider: &str, model: &str) {
    roll_up(hub, u, &key.id, provider, model, 1, 0, 0);
    let api = hub.api(); let now = pgm_core_internal::time::iso_now();
    let mut first_today = false; let mut prefix = String::new();
    api.edit(u, |rec| {
        let bump = |k: &mut Value, first: &mut bool, prefix: &mut String| { *first = s(k, "lastUsed").get(..10) != now.get(..10); *prefix = s(k, "prefix"); k["calls"] = json!(k["calls"].as_u64().unwrap_or(0) + 1); k["lastUsed"] = json!(now); };
        if !key.named { bump(rec, &mut first_today, &mut prefix); }
        else if let Some(k) = rec["keys"].as_array_mut().and_then(|a| a.iter_mut().find(|k| k["id"] == json!(key.id))) { bump(k, &mut first_today, &mut prefix); }
    });
    if first_today { pgm_core_internal::audit::record(u, "apikey.use", &key.id, json!({ "prefix": prefix, "label": key.label }), ""); }
}
/// Adds an answer's tokens to its key's counters (`tokensIn`: everything read, cache included; `tokensOut`) and to the key's
/// day roll-up under the provider and model it was routed to.
#[allow(clippy::too_many_arguments)]
pub(crate) fn count_tokens(hub: &dyn AiHub, u: &str, key: &KeyCtx, provider: &str, model: &str, input: u64, output: u64) {
    hub.api().edit(u, |rec| {
        let add = |k: &mut Value| { k["tokensIn"] = json!(k["tokensIn"].as_u64().unwrap_or(0) + input); k["tokensOut"] = json!(k["tokensOut"].as_u64().unwrap_or(0) + output); };
        if !key.named { add(rec); }
        else if let Some(k) = rec["keys"].as_array_mut().and_then(|a| a.iter_mut().find(|k| k["id"] == json!(key.id))) { add(k); }
    });
    roll_up(hub, u, &key.id, provider, model, 0, input, output);
}

// ---------- per-key usage by day (G5): <root>/profiles/<user>/apikey-usage.json ----------
// { <key id>: { "YYYY-MM-DD" (UTC): { calls, tokensIn, tokensOut, models: { "<provider> <model>": { provider, model, calls,
// tokensIn, tokensOut } } } } }. Written by the Rust hub only (the frozen JS hub neither reads nor needs it), add-only next to
// the totals on the key record; it lives in the person's profile folder, which is data (never shipped, in every deploy /
// pack / restore / hand-off list already). Days older than USAGE_KEEP_DAYS are dropped when the file is written; a revoked
// key's days stay until then but are never listed.
const USAGE_KEEP_DAYS: i64 = 120;
fn usage_file(hub: &dyn AiHub, u: &str) -> PathBuf { hub.root().join("profiles").join(u).join("apikey-usage.json") }
fn utc_day(back: i64) -> String { (chrono::Utc::now() - chrono::Duration::days(back)).format("%Y-%m-%d").to_string() }
#[allow(clippy::too_many_arguments)]
fn roll_up(hub: &dyn AiHub, u: &str, key: &str, provider: &str, model: &str, calls: u64, tin: u64, tout: u64) {
    if !pgm_identity_internal::valid_username(u) || key.is_empty() || (calls == 0 && tin == 0 && tout == 0) { return; }
    let _one = hub.api().roll.lock().unwrap_or_else(|e| e.into_inner()); // one read-change-write at a time
    let f = usage_file(hub, u);
    let mut all = read_json(&f).filter(Value::is_object).unwrap_or_else(|| json!({}));
    let add = |o: &mut Value| { for (k, n) in [("calls", calls), ("tokensIn", tin), ("tokensOut", tout)] { o[k] = json!(o[k].as_u64().unwrap_or(0) + n); } };
    let d = &mut all[key][utc_day(0)];
    add(d);
    let m = &mut d["models"][format!("{provider} {model}")];
    m["provider"] = json!(provider); m["model"] = json!(model); add(m);
    let cut = utc_day(USAGE_KEEP_DAYS);
    if let Some(keys) = all.as_object_mut() { for v in keys.values_mut() { if let Some(days) = v.as_object_mut() { days.retain(|day, _| day.as_str() >= cut.as_str()); } } }
    if let Some(p) = f.parent() { let _ = std::fs::create_dir_all(p); }
    write_json(&f, &all);
}
/// `GET /api/apikey/usage?days=N` (1 to 90, default 30): the person's own keys with their totals and, for the last N days
/// (UTC, oldest first, empty days included), the calls and tokens per day and per provider and model.
fn usage_view(hub: &dyn AiHub, u: &str, days: i64) -> Value {
    let rec = hub.api().key_of(u);
    let roll = read_json(&usage_file(hub, u)).unwrap_or(Value::Null);
    let day_list: Vec<String> = (0..days).rev().map(utc_day).collect();
    let mut keys: Vec<(String, String, Value)> = vec![];
    if !s(&rec, "hash").is_empty() { keys.push(("default".into(), "default".into(), rec.clone())); }
    for k in rec["keys"].as_array().cloned().unwrap_or_default() { keys.push((s(&k, "id"), s(&k, "label"), k)); }
    let n = |v: &Value, k: &str| v[k].as_u64().unwrap_or(0);
    let out: Vec<Value> = keys.into_iter().map(|(id, label, k)| {
        let r = &roll[&id];
        let by_day: Vec<Value> = day_list.iter().map(|d| { let x = &r[d]; json!({ "day": d, "calls": n(x, "calls"), "tokensIn": n(x, "tokensIn"), "tokensOut": n(x, "tokensOut") }) }).collect();
        let mut models: Vec<Value> = vec![];
        for d in &day_list {
            for m in r[d]["models"].as_object().map(|o| o.values().cloned().collect::<Vec<_>>()).unwrap_or_default() {
                match models.iter_mut().find(|x| x["provider"] == m["provider"] && x["model"] == m["model"]) {
                    Some(x) => { for f in ["calls", "tokensIn", "tokensOut"] { x[f] = json!(n(x, f) + n(&m, f)); } }
                    None => models.push(json!({ "model": m["model"], "provider": m["provider"], "calls": n(&m, "calls"), "tokensIn": n(&m, "tokensIn"), "tokensOut": n(&m, "tokensOut") })),
                }
            }
        }
        models.sort_by_key(|x| std::cmp::Reverse((n(x, "calls"), n(x, "tokensIn") + n(x, "tokensOut"))));
        let sum = |f: &str| by_day.iter().map(|x| n(x, f)).sum::<u64>();
        json!({ "id": id, "label": label, "prefix": s(&k, "prefix"), "calls": n(&k, "calls"), "tokensIn": n(&k, "tokensIn"), "tokensOut": n(&k, "tokensOut"), "lastUsed": k.get("lastUsed").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null),
            "period": { "calls": sum("calls"), "tokensIn": sum("tokensIn"), "tokensOut": sum("tokensOut") }, "byDay": by_day, "byModel": models })
    }).collect();
    json!({ "days": days, "from": day_list.first(), "to": day_list.last(), "keys": out })
}
fn total(u: &Value) -> Value { let mut v = u.clone(); v["total_tokens"] = json!(u["prompt_tokens"].as_u64().unwrap_or(0) + u["completion_tokens"].as_u64().unwrap_or(0)); v }

/// /v1/*: the API itself (its own keys, no browser session).
pub async fn v1(hub: &Arc<dyn AiHub>, r: &Req) -> Resp {
    let p = r.path.as_str();
    if r.method == hyper::Method::OPTIONS { return cors(hyper::Response::builder().status(204).body(pgm_net_internal::http::full("")).expect("response")); }
    let anthropic = p.starts_with("/v1/messages") || (p == "/v1/models" && r.header("anthropic-version").is_some());
    let fail = |st: u16, m: &str, t: &str| if anthropic { ant_error(st, m, t) } else { oa_error(st, m, t) };
    let (user, key) = match hub.api().account_for_key(r) { Ok(u) => u, Err(e) => return fail(401, &e, "authentication_error") };
    v1_key(hub, r, user, key).await
}
/// The API as that account, who the caller already is (a key checked by [`v1`], or a gateway product's signed request
/// for a person it linked): the same answers, the same providers and sharing, as their default key.
pub async fn v1_as(hub: &Arc<dyn AiHub>, r: &Req, user: String) -> Resp { v1_key(hub, r, user, KeyCtx::default_key()).await }

fn slug(t: &str) -> String { let mut o = String::new(); for c in t.to_lowercase().chars() { if c.is_alphanumeric() { o.push(c); } else if !o.ends_with('-') { o.push('-'); } } o.trim_matches('-').to_string() }
/// What a provider is called in `<provider>/<model>`: its host's nickname, else its label.
fn alias_of(p: &Value) -> String { let n = s(p, "nickname"); let n = if n.is_empty() { s(p, "label") } else { n }; slug(&n) }
fn is_usable(p: &Value) -> bool { p["online"] != json!(false) && s(p, "capped").is_empty() }
/// The providers this key may answer from, in the order a plain model id is looked up (the key's pinned provider, then
/// the person's own, then shared ones), each with only the models the person and the key may use for the API: a shared
/// provider's scope (the API use, its models) and the key's own `providers` / `models` narrow it, never widen it.
fn api_offers(hub: &Arc<dyn AiHub>, user: &str, a: &Value, key: &KeyCtx) -> Vec<Value> {
    let (w, ai) = (hub.work(), hub.ai());
    let mut all = crate::work::providers_list(&**hub, user);
    let pinned: Vec<String> = if key.named { key.providers.clone() } else { vec![s(a, "provider")] };
    let mut head: Vec<Value> = vec![];
    for id in &pinned {
        let p = all.iter().find(|x| x["id"] == json!(id)).cloned().or_else(|| if id.is_empty() { None } else { w.provider_get(ai, id, user) });
        if let Some(p) = p { if !head.iter().any(|x| x["id"] == p["id"]) { head.push(p); } }
    }
    if !key.named && head.is_empty() { if let Some(p) = w.default_for(ai, user) { head.push(all.iter().find(|x| x["id"] == p["id"]).cloned().unwrap_or(p)); } }
    all.retain(|x| !head.iter().any(|h| h["id"] == x["id"]));
    all.sort_by_key(|x| x["owner"] != json!(user)); // stable: own first, shared after
    // the default key bound to a provider (Settings → API access) answers from that one alone, as it always did; a key
    // with no binding, and every named key, route across everything they may use
    if key.named || s(a, "provider").is_empty() || head.is_empty() { head.extend(all); }
    head.into_iter().filter_map(|mut p| {
        let id = s(&p, "id");
        if matches!(p["kind"].as_str(), Some("host-jcode") | Some("jcode")) { return None; }
        if key.named && !key.providers.is_empty() && !key.providers.contains(&id) { return None; }
        if id.starts_with("h:") && ai.scope_check(user, &id, "api", "").is_err() { return None; }
        let ms: Vec<Value> = p["models"].as_array().cloned().unwrap_or_default().into_iter().filter(|m| m.as_str().map(|m| (key.models.is_empty() || key.models.iter().any(|k| k == m)) && (!id.starts_with("h:") || ai.scope_check(user, &id, "api", m).is_ok())).unwrap_or(false)).collect();
        if ms.is_empty() { return None; }
        p["models"] = json!(ms); Some(p)
    }).collect()
}
/// The provider and model a request's `model` names: a plain id (the first usable provider offering it) or
/// `<provider>/<model>`.
fn route_model(offers: &[Value], asked: &str) -> Option<(Value, String)> {
    let has = |p: &Value, m: &str| p["models"].as_array().map(|a| a.iter().any(|x| x == &json!(m))).unwrap_or(false);
    let first = |v: Vec<&Value>| v.iter().find(|p| is_usable(p)).or(v.first()).map(|p| (*p).clone());
    if let Some(p) = first(offers.iter().filter(|p| has(p, asked)).collect()) { return Some((p, asked.to_string())); }
    let (pre, m) = asked.split_once('/')?;
    let pre = slug(pre);
    first(offers.iter().filter(|p| has(p, m) && (alias_of(p) == pre || slug(&s(p, "label")) == pre || slug(&s(p, "id")) == pre)).collect()).map(|p| (p, m.to_string()))
}
/// Every model once (plain id, the first provider that offers it) with the provider's label, and a `<provider>/<model>`
/// entry for each provider when two offer the same id: (id, owned_by).
fn list_models(offers: &[Value]) -> Vec<(String, String)> {
    let owned = |p: &Value| if p["kind"] == json!("host") { "mills".to_string() } else { s(p, "label") };
    let mut seen: Vec<String> = vec![]; let mut out = vec![];
    for p in offers { for m in p["models"].as_array().cloned().unwrap_or_default().iter().filter_map(|m| m.as_str()) { if !seen.iter().any(|x| x == m) { seen.push(m.to_string()); out.push((m.to_string(), owned(p))); } } }
    for m in seen.iter() { let who: Vec<&Value> = offers.iter().filter(|p| p["models"].as_array().map(|a| a.iter().any(|x| x == &json!(m))).unwrap_or(false)).collect(); if who.len() > 1 { for p in who { out.push((format!("{}/{m}", alias_of(p)), owned(p))); } } }
    out
}
async fn v1_key(hub: &Arc<dyn AiHub>, r: &Req, user: String, key: KeyCtx) -> Resp {
    let p = r.path.as_str();
    if r.method == hyper::Method::OPTIONS { return cors(hyper::Response::builder().status(204).body(pgm_net_internal::http::full("")).expect("response")); }
    let anthropic = p.starts_with("/v1/messages") || (p == "/v1/models" && r.header("anthropic-version").is_some());
    let fail = |st: u16, m: &str, t: &str| if anthropic { ant_error(st, m, t) } else { oa_error(st, m, t) };
    let api = hub.api();
    let a = api.key_of(&user);
    let body: Option<Value> = if r.body.is_empty() { Some(json!({})) } else { serde_json::from_slice(&r.body).ok() };
    let asked = if r.method == hyper::Method::GET { "" } else { body.as_ref().and_then(|b| b["model"].as_str()).unwrap_or("") };
    let offers = api_offers(hub, &user, &a, &key);
    // which provider and model answers: what `model` names across everything this key may use; the default key's bound
    // provider / model still answer when it names none we know (and a provider shared with a scope the API or the model
    // is not in refuses, naming the owner); a named key refuses a model it may not use
    let routed = if asked.is_empty() { None } else { route_model(&offers, asked) };
    let legacy = || -> Result<(Value, String), Resp> {
        let Some(prov) = hub.work().effective(hub.ai(), &user, &s(&a, "provider")) else { return Err(fail(503, "No AI is available: this hub has no Claude of its own and no AI host offering one is connected.", "service_unavailable")) };
        if s(&prov, "id").starts_with("h:") { if let Err(m) = hub.ai().scope_check(&user, &s(&prov, "id"), "api", asked) { return Err(fail(403, &m, "permission_error")); } }
        let models: Vec<String> = str_list(&prov["models"]);
        let m = pick_model(&prov, &Some(asked).filter(|m| models.iter().any(|x| x == m)).map(String::from).unwrap_or_else(|| s(&a, "model")));
        Ok((prov, m))
    };
    let chosen: Result<(Value, String), Resp> = match routed {
        Some(x) => Ok(x),
        None if key.named => {
            if !asked.is_empty() { let names: Vec<String> = list_models(&offers).into_iter().map(|x| x.0).collect(); Err(fail(404, &format!("The model \"{asked}\" is not available to this key{}.", if names.is_empty() { String::new() } else { format!(" (it may use: {})", names.join(", ")) }), "not_found_error")) }
            else if let Some(p) = offers.first() { let m = p["models"][0].as_str().unwrap_or("").to_string(); Ok((p.clone(), m)) }
            else { Err(fail(403, NO_KEY_USE, "permission_error")) }
        }
        None => legacy(),
    };
    match (r.method.as_str(), p) {
        ("GET", "/v1/models") => {
            let list = if !offers.is_empty() { list_models(&offers) } else if key.named { return fail(403, NO_KEY_USE, "permission_error") } else { match legacy() { Ok((prov, _)) => str_list(&prov["models"]).into_iter().map(|x| (x, if prov["kind"] == json!("host") { "mills".to_string() } else { s(&prov, "owner") })).collect(), Err(e) => return e } };
            if anthropic { return jout(200, json!({ "data": list.iter().map(|(m, _)| json!({ "type": "model", "id": m, "display_name": m, "created_at": "1970-01-01T00:00:00Z" })).collect::<Vec<_>>(), "has_more": false, "first_id": list.first().map(|x| &x.0), "last_id": list.last().map(|x| &x.0) })); }
            jout(200, json!({ "object": "list", "data": list.iter().map(|(m, o)| json!({ "id": m, "object": "model", "created": 0, "owned_by": o })).collect::<Vec<_>>() }))
        }
        ("POST", "/v1/messages/count_tokens") => {
            let Some(b) = body else { return ant_error(400, "Body is not JSON.", "invalid_request_error") };
            let (system, msgs) = from_anthropic(&b);
            let chars = js_len(&system) + msgs.iter().map(|m| js_len(&m.1)).sum::<usize>() + crate::gateway::tools_chars(&b); // a rough count: the tool definitions count too
            jout(200, json!({ "input_tokens": chars.div_ceil(4).max(1) }))
        }
        ("POST", "/v1/messages") => {
            let Some(b) = body else { return ant_error(400, "Body is not JSON.", "invalid_request_error") };
            let (system, msgs) = from_anthropic(&b);
            if msgs.is_empty() { return ant_error(400, "messages: at least one user or assistant message is required.", "invalid_request_error"); }
            let (prov, model) = match chosen { Ok(x) => x, Err(e) => return e };
            // an Anthropic-speaking provider (API key, a host's Claude login) gets the request raw (G2, gateway.rs); the text
            // path below stays for the ones that speak neither; an OpenAI-speaking one gets it translated (G3)
            if crate::gateway::speaks_anthropic(&prov) { return crate::gateway::messages(hub, r, &user, &key, &prov, &model, b).await; }
            if crate::gateway::speaks_openai(&prov) { return crate::gateway::messages_via_openai(hub, &user, &key, &prov, &model, b).await; }
            let Some(run) = start_running(hub, &user, &key) else { return too_many(true, &key, max_concurrent(&**hub, &key)) };
            let max = b["max_tokens"].as_f64().filter(|n| *n > 0.0).map(|n| (n.round() as u64).min(128000));
            count_use(&**hub, &user, &key, &s(&prov, "id"), &model);
            let mid = format!("msg_{}", pgm_core_internal::crypto::random_hex(12));
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
            let (prov, model) = match chosen { Ok(x) => x, Err(e) => return e };
            // G3 (gateway.rs): an OpenAI-speaking provider gets the request raw; an Anthropic one gets it translated (always for
            // an API key, for a Claude login when it carries tools or images: a plain text one keeps the path below)
            if crate::gateway::speaks_openai(&prov) { return crate::gateway::chat_raw(hub, &user, &key, &prov, &model, b).await; }
            if crate::gateway::speaks_anthropic(&prov) && (prov["kind"] == json!("anthropic") || crate::xlate::needs_rich(&b)) { return crate::gateway::chat_via_anthropic(hub, r, &user, &key, &prov, &model, b).await; }
            let Some(run) = start_running(hub, &user, &key) else { return too_many(false, &key, max_concurrent(&**hub, &key)) };
            let system = msgs.iter().filter(|m| m.0 == "system").map(|m| m.1.clone()).collect::<Vec<_>>().join("\n\n");
            let max = b["max_tokens"].as_u64();
            count_use(&**hub, &user, &key, &s(&prov, "id"), &model);
            let cid = format!("chatcmpl-{}", pgm_core_internal::crypto::random_hex(12)); let created = now_ms() / 1000;
            let pid = s(&prov, "id");
            if crate::toolconv::wants_tools(&b) { // T-317: tools in, tool calls out (a request without them takes the path below, unchanged)
                if let Some(m) = crate::toolconv::check_tools(&b) { return oa_error(400, &m, "invalid_request_error"); }
                let res = complete_tools(hub, &user, &pid, &model, &b, run.cancel.clone(), max).await;
                drop(run);
                return match res {
                    Ok(an) => tools_reply(an, model, &cid, created, b["stream"].as_bool().unwrap_or(false)),
                    Err(e) => oa_error(if e.status == 0 { 502 } else { e.status }, &e.msg, if e.status == 503 { "service_unavailable" } else if e.status == 400 { "invalid_request_error" } else { "api_error" }),
                };
            }
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
        // G5: the Responses API (Codex): raw to a provider that speaks it, translated to the others (gateway.rs)
        ("POST", "/v1/responses") => {
            let Some(b) = body.filter(Value::is_object) else { return oa_error(400, "Body is not JSON.", "invalid_request_error") };
            let (prov, model) = match chosen { Ok(x) => x, Err(e) => return e };
            crate::gateway::responses(hub, r, &user, &key, &prov, &model, b).await
        }
        _ => fail(404, "Unknown endpoint. This hub serves GET /v1/models, POST /v1/chat/completions and POST /v1/responses (OpenAI) and POST /v1/messages (Anthropic).", "not_found_error"),
    }
}

/// Settings → API access and → AI providers (signed in as `a`).
pub fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    let api = hub.api(); let u = &a.username; let w = hub.work();
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let b = r.json();
    let list = || json!(crate::work::providers_list(&**hub, u));
    Some(match r.path.as_str() {
        "/api/apikey" if get => answer(200, api.status(u)),
        // G5: the signed-in person's own keys, their calls and tokens per day and per model (never another person's)
        "/api/apikey/usage" if get => { let d = r.param("days").and_then(|x| x.trim().parse::<i64>().ok()).unwrap_or(30).clamp(1, 90); answer(200, usage_view(&**hub, u, d)) }
        "/api/apikey/rotate" if post => {
            let key = format!("mills_{}", pgm_core_internal::crypto::random_hex(24));
            let mut k = api.key_of(u); if !k.is_object() { k = json!({}); }
            let days = if b.get("expiresDays").map(|v| !v.is_null()).unwrap_or(false) { clamp_days(&b["expiresDays"]) } else { clamp_days(&k["expiresDays"]) };
            k["hash"] = json!(pgm_core_internal::crypto::sha256_hex(key.as_bytes())); k["prefix"] = json!(&key[..12]); k["createdAt"] = json!(pgm_core_internal::time::iso_now()); k["expiresDays"] = json!(days); k["calls"] = json!(0);
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
                let Some(p) = w.provider_get(hub.ai(), &want, u) else { return Some(answer(400, json!({ "error": "Unknown provider." }))) };
                k["provider"] = p["id"].clone();
                if !p["models"].as_array().map(|m| m.contains(&k["model"])).unwrap_or(false) { k["model"] = json!(""); }
            }
            if b.get("model").is_some() {
                let p = w.effective(hub.ai(), u, &s(&k, "provider"));
                k["model"] = if p.map(|p| p["models"].as_array().map(|m| m.contains(&b["model"])).unwrap_or(false)).unwrap_or(false) { b["model"].clone() } else { json!("") };
            }
            api.set_key(u, Some(k));
            answer(200, api.status(u))
        }
        // another key next to the default one (a label, an expiry, optionally narrowed to some providers / models and its own
        // concurrency limit); the key is in the answer once
        "/api/apikey/create" if post => {
            let label: String = s(&b, "label").trim().chars().take(60).collect();
            let label = if label.is_empty() { "key".to_string() } else { label };
            if api.key_of(u)["keys"].as_array().map(|k| k.len()).unwrap_or(0) >= 20 { return Some(answer(400, json!({ "error": "That is the limit: 20 extra keys. Revoke one first." }))); }
            let providers = match b.get("providers") { None | Some(Value::Null) => vec![], Some(v) => str_list(v) };
            for id in &providers {
                if w.provider_get(hub.ai(), id, u).is_none() { return Some(answer(400, json!({ "error": format!("You may not use the provider \"{id}\" (or it does not exist), so a key cannot be limited to it.") }))); }
                if let Err(m) = hub.ai().scope_check(u, id, "api", "") { return Some(answer(400, json!({ "error": m }))); }
            }
            let models = match b.get("models") { None | Some(Value::Null) => vec![], Some(v) => str_list(v) };
            if !models.is_empty() {
                let offered = api_offers(hub, u, &json!({}), &KeyCtx { named: true, providers: providers.clone(), ..Default::default() });
                let all: Vec<String> = offered.iter().flat_map(|p| str_list(&p["models"])).collect();
                if let Some(m) = models.iter().find(|m| !all.contains(m)) { return Some(answer(400, json!({ "error": format!("The model \"{m}\" is not one you may use for the API{}, so a key cannot be limited to it.", if providers.is_empty() { "" } else { " on those providers" }) }))); }
            }
            let max = b["maxConcurrent"].as_f64().filter(|n| *n >= 1.0).map(|n| (n.round() as u64).min(64));
            let key = format!("mills_{}", pgm_core_internal::crypto::random_hex(24));
            let id = format!("k-{}", pgm_core_internal::crypto::random_hex(4));
            let mut rec = json!({ "id": id, "label": label, "hash": pgm_core_internal::crypto::sha256_hex(key.as_bytes()), "prefix": &key[..12], "createdAt": pgm_core_internal::time::iso_now(), "expiresDays": clamp_days(&b["expiresDays"]), "lastUsed": Value::Null, "calls": 0, "enabled": true });
            if !providers.is_empty() { rec["providers"] = json!(providers); }
            if !models.is_empty() { rec["models"] = json!(models); }
            if let Some(m) = max { rec["maxConcurrent"] = json!(m); }
            let view = key_view(&rec, &id, &label);
            api.edit(u, |r| { if !r["keys"].is_array() { r["keys"] = json!([]); } if let Some(a) = r["keys"].as_array_mut() { a.push(rec); } });
            let mut st = api.status(u); st["created"] = view; st["id"] = json!(id);
            let mut o = Map::new(); o.insert("key".into(), json!(key)); for (k2, v) in st.as_object().cloned().unwrap_or_default() { o.insert(k2, v); }
            answer(200, Value::Object(o))
        }
        // one key by id; without an id (or "default") the person's default key, as it always was
        "/api/apikey/revoke" if post => {
            let id = s(&b, "id");
            if id.is_empty() || id == "default" {
                api.edit(u, |r| { if let Some(m) = r.as_object_mut() { for k in ["hash", "prefix", "createdAt", "expiresDays", "calls", "lastUsed", "provider", "model"] { m.remove(k); } if m.get("keys").and_then(Value::as_array).map(|a| a.is_empty()).unwrap_or(true) { m.remove("keys"); } } });
            } else {
                if api.named_key(u, &id).is_none() { return Some(answer(404, json!({ "error": "No such key." }))); }
                api.edit(u, |r| { if let Some(a) = r["keys"].as_array_mut() { a.retain(|k| k["id"] != json!(id)); } if r["keys"].as_array().map(|a| a.is_empty()).unwrap_or(false) { if let Some(m) = r.as_object_mut() { m.remove("keys"); } } });
            }
            answer(200, api.status(u))
        }
        "/api/providers" if get => { let p = s(&w.state(a), "provider"); answer(200, json!({ "providers": list(), "provider": if p.is_empty() { "host".to_string() } else { p } })) }
        "/api/providers/add" if post => match add_provider(&**hub, u, &b) { Ok(p) => answer(200, json!({ "provider": p, "providers": list() })), Err(e) => answer(400, json!({ "error": e })) },
        "/api/providers/update" if post => match update_provider(&**hub, u, &s(&b, "id"), &b) { Ok(p) => answer(200, json!({ "provider": p, "providers": list() })), Err(e) => answer(400, json!({ "error": e })) },
        "/api/providers/remove" if post => match remove_provider(&**hub, u, &s(&b, "id")) { Ok(()) => answer(200, json!({ "providers": list() })), Err(e) => answer(400, json!({ "error": e })) },
        // Settings → My AI hosts' list (+ the default provider, which this module knows)
        "/api/hosts" if get => {
            let ai = hub.ai();
            let all: Vec<Value> = ai.all().into_iter().map(|h| { let owner = h["user"] == json!(u.as_str()); json!({ "id": h["id"], "nickname": h["nickname"], "user": h["user"], "online": h["online"], "providers": h["providers"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| owner || x["allowedUsers"].as_array().map(|l| l.contains(&json!(u.as_str()))).unwrap_or(false)).map(|x| json!({ "id": x["id"], "kind": x["kind"], "label": x["label"], "models": x["models"], "capped": x["capped"] })).collect::<Vec<_>>() }) }).collect();
            answer(200, json!({ "mine": ai.mine(u), "all": all, "defaultProvider": w.default_for(ai, u).map(|p| s(&p, "id")).unwrap_or_default(), "group": hub.cfg().get("hostGroup").cloned().unwrap_or(json!([])) }))
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgm_context_internal::ledger::Usage as L;

    /// One sample of each provider kind's usage, as `complete` leaves it in `Answer.native`: the same ledger shape.
    #[test]
    fn every_provider_kinds_usage_normalises_to_the_ledger_shape() {
        // openai / host-openai: prompt_tokens includes the cached ones
        assert_eq!(normalise(&json!({ "prompt_tokens": 1000, "completion_tokens": 20, "total_tokens": 1020, "prompt_tokens_details": { "cached_tokens": 900 } })), Some(L { input: 100, output: 20, cache_read: 900, cache_write: 0 }));
        // OpenAI-compatible without cache details (Ollama and friends)
        assert_eq!(normalise(&json!({ "prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7 })), Some(L { input: 3, output: 4, cache_read: 0, cache_write: 0 }));
        // anthropic / host-claude: input_tokens is the uncached part, cache read / creation are apart
        assert_eq!(normalise(&json!({ "input_tokens": 12, "output_tokens": 30, "cache_read_input_tokens": 4000, "cache_creation_input_tokens": 250 })), Some(L { input: 12, output: 30, cache_read: 4000, cache_write: 250 }));
        // host-claude (legacy) and the cli: the Claude Code assistant message's usage is Anthropic's shape too
        assert_eq!(normalise(&json!({ "input_tokens": 5, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 0, "output_tokens": 8, "service_tier": "standard" })), Some(L { input: 5, output: 8, cache_read: 0, cache_write: 100 }));
        // host-codex: the Responses API counts input_tokens with the cached ones inside
        let codex = responses_native(&json!({ "input_tokens": 2000, "output_tokens": 50, "input_tokens_details": { "cached_tokens": 1536 } }));
        assert_eq!(normalise(&codex), Some(L { input: 464, output: 50, cache_read: 1536, cache_write: 0 }));
        assert_eq!(normalise(&responses_native(&json!({ "input_tokens": 2, "output_tokens": 3 }))), Some(L { input: 2, output: 3, cache_read: 0, cache_write: 0 }));
        // Gemini / OpenRouter shapes come through the same ledger
        assert_eq!(normalise(&json!({ "promptTokenCount": 1000, "candidatesTokenCount": 20, "cachedContentTokenCount": 900 })), Some(L { input: 100, output: 20, cache_read: 900, cache_write: 0 }));
        assert_eq!(normalise(&json!({ "prompt_tokens": 500, "completion_tokens": 5, "prompt_tokens_details": { "cached_tokens": 100 }, "cache_discount": 0.002 })), Some(L { input: 400, output: 5, cache_read: 100, cache_write: 0 }));
        // no usage → nothing noted
        assert_eq!(normalise(&Value::Null), None);
        assert_eq!(normalise(&json!({})), None);
    }

    fn prov(id: &str, nick: &str, owner: &str, models: &[&str]) -> Value { json!({ "id": id, "kind": "host-claude", "label": id, "nickname": nick, "owner": owner, "online": true, "capped": "", "models": models }) }

    #[test]
    fn a_model_routes_by_plain_id_or_provider_alias_and_is_listed_once() {
        let offers = vec![prov("own", "Mine", "me", &["opus", "sonnet"]), prov("h:g/c", "Gee Node", "gee", &["opus", "fable"])];
        assert_eq!(route_model(&offers, "opus").map(|x| (s(&x.0, "id"), x.1)), Some(("own".into(), "opus".into())));
        assert_eq!(route_model(&offers, "fable").map(|x| s(&x.0, "id")), Some("h:g/c".into()));
        assert_eq!(route_model(&offers, "gee-node/opus").map(|x| (s(&x.0, "id"), x.1)), Some(("h:g/c".into(), "opus".into())));
        assert_eq!(route_model(&offers, "mine/sonnet").map(|x| s(&x.0, "id")), Some("own".into()));
        assert!(route_model(&offers, "mine/fable").is_none() && route_model(&offers, "nope").is_none());
        let l = list_models(&offers); let ids: Vec<&str> = l.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(ids, vec!["opus", "sonnet", "fable", "mine/opus", "gee-node/opus"]);
        assert_eq!(l[2].1, "h:g/c"); // owned_by = the provider's label
        // an offline provider is passed over for one that is up
        let mut o2 = offers.clone(); o2[0]["online"] = json!(false);
        assert_eq!(route_model(&o2, "opus").map(|x| s(&x.0, "id")), Some("h:g/c".into()));
    }
}
