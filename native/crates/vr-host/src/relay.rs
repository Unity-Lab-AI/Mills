//! Login relays, the host's side (bridge/host-relay.js). The hub's own Claude Code / Codex run on the server; only their
//! model calls come here, and this machine adds its own login and forwards them to ONE fixed provider address:
//!   claude-relay  the Claude login (<configDir>/.credentials.json)  → https://api.anthropic.com  /v1/messages[/count_tokens]
//!   codex-relay   the Codex CLI's ChatGPT login (<codexHome>/auth.json) → https://chatgpt.com/backend-api/codex  /responses, /models
//! The token never leaves this machine; the caller's own auth headers are dropped; only a short list of headers passes.
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::time::Duration;

use crate::files::log;
use crate::runner::H;
use vr_core::host::Job;
use vr_core::settings::Provider;
use vr_core::{now_ms, Error, Result};

/// Whose logins: VR_LOGIN_HOME (the Windows service runs as LocalSystem and is told the installing person's profile),
/// else this account's home.
fn home() -> PathBuf { std::env::var_os("VR_LOGIN_HOME").or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")) }
pub struct Relay { pub base: &'static str, pub name: &'static str }
pub fn relay_of(kind: &str) -> Option<Relay> {
    match kind { "claude-relay" => Some(Relay { base: "https://api.anthropic.com", name: "Claude" }), "codex-relay" => Some(Relay { base: "https://chatgpt.com/backend-api/codex", name: "Codex" }), _ => None }
}
fn dir_of(p: &Provider) -> PathBuf {
    if p.relay_kind() == "claude-relay" { if p.config_dir.is_empty() { home().join(".claude") } else { PathBuf::from(&p.config_dir) } }
    else if p.codex_home.is_empty() { home().join(".codex") } else { PathBuf::from(&p.codex_home) }
}
/// The paths a relay forwards: Claude /v1/messages and /v1/messages/count_tokens, Codex /responses and /models
/// (each with an optional plain query), nothing else.
pub fn path_ok(kind: &str, path: &str) -> bool {
    let (p, q) = path.split_once('?').map(|(a, b)| (a, Some(b))).unwrap_or((path, None));
    let q_ok = q.map(|q| q.chars().all(|c| c.is_ascii_alphanumeric() || "_=&.-".contains(c))).unwrap_or(true);
    q_ok && match kind { "claude-relay" => p == "/v1/messages" || p == "/v1/messages/count_tokens", "codex-relay" => p == "/responses" || p == "/models", _ => false }
}
fn pass_req(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    matches!(k.as_str(), "content-type" | "accept" | "anthropic-version" | "anthropic-beta" | "openai-beta" | "session_id" | "originator" | "x-app") || k.starts_with("x-stainless-")
}
fn pass_res(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    matches!(k.as_str(), "content-type" | "request-id" | "retry-after") || k.starts_with("anthropic-ratelimit-") || k.starts_with("x-codex-") || k.starts_with("openai-")
}
pub fn pass_headers(h: &reqwest::header::HeaderMap) -> Map<String, Value> {
    h.iter().filter(|(k, _)| pass_res(k.as_str())).filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), json!(v)))).collect()
}
/// Anthropic reports the subscription's windows in its response headers.
pub fn usage_from(h: &reqwest::header::HeaderMap) -> Option<Value> {
    let g = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(String::from);
    let w = |name: &str| -> Option<Value> {
        let u = g(&format!("anthropic-ratelimit-unified-{name}-utilization"))?;
        let r = g(&format!("anthropic-ratelimit-unified-{name}-reset")).and_then(|r| r.parse::<f64>().ok()).map(|r| r * 1000.0);
        Some(json!({ "utilization": u.parse::<f64>().ok(), "resetsAt": r, "status": g(&format!("anthropic-ratelimit-unified-{name}-status")).unwrap_or_default() }))
    };
    let (five, seven) = (w("5h"), w("7d"));
    if five.is_none() && seven.is_none() { return None; }
    let mut o = Map::new();
    if let Some(f) = five { o.insert("five_hour".into(), f); }
    if let Some(s) = seven { o.insert("seven_day".into(), s); }
    Some(Value::Object(o))
}
struct Login { token: String, expires_at: u64, account: String, api_key: String }
fn login_token(p: &Provider) -> Result<Login> {
    let dir = dir_of(p);
    if p.relay_kind() == "claude-relay" {
        let j: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".credentials.json")).map_err(|_| Error::status(503, "This host has no Claude login (sign in to it on this machine once)."))?)?;
        let o = &j["claudeAiOauth"];
        return Ok(Login { token: o["accessToken"].as_str().unwrap_or("").into(), expires_at: o["expiresAt"].as_u64().unwrap_or(0), account: String::new(), api_key: String::new() });
    }
    let j: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("auth.json")).map_err(|_| Error::status(503, "This host has no Codex login (sign in to it on this machine once)."))?)?;
    Ok(Login { token: j["tokens"]["access_token"].as_str().unwrap_or("").into(), expires_at: 0, account: j["tokens"]["account_id"].as_str().unwrap_or("").into(), api_key: j["OPENAI_API_KEY"].as_str().unwrap_or("").into() })
}
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// Lets the provider's own CLI refresh its login (it does when it runs and finds the token old), in an empty folder.
async fn refresh_login(h: &H, p: &Provider) {
    let _g = REFRESH.lock().await;
    let sandbox = h.folder.sandbox(); let _ = std::fs::create_dir_all(&sandbox);
    let claude = p.relay_kind() == "claude-relay";
    let cmd = if !p.command.is_empty() { p.command.clone() } else if claude { h.cfg.work().get("claudeCommand").and_then(Value::as_str).unwrap_or("claude").to_string() } else { "codex".into() };
    let args: Vec<&str> = if claude { vec!["-p", "--strict-mcp-config", "--max-turns", "1", "--model", "claude-haiku-5-5"] } else { vec!["exec", "--skip-git-repo-check", "-s", "read-only", "Reply with OK."] };
    log(format!("refreshing the {} login", if claude { "Claude" } else { "Codex" }));
    let mut c = crate::proc::command(&cmd);
    c.args(&args).current_dir(&sandbox).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .env_remove("ANTHROPIC_BASE_URL").env_remove("ANTHROPIC_AUTH_TOKEN").env_remove("ANTHROPIC_API_KEY")
        .env(if claude { "CLAUDE_CONFIG_DIR" } else { "CODEX_HOME" }, dir_of(p));
    if let Ok(mut child) = c.spawn() {
        if let Some(mut si) = child.stdin.take() { use tokio::io::AsyncWriteExt; let _ = si.write_all(if claude { b"Reply with OK." } else { b"" }).await; }
        let _ = tokio::time::timeout(Duration::from_secs(120), child.wait()).await;
        let _ = child.kill().await;
    }
}
/// One forwarded call.
pub async fn forward(h: &H, p: &Provider, job: &Job) -> Result<reqwest::Response> {
    let kind = p.relay_kind();
    let r = relay_of(kind).ok_or_else(|| Error::status(404, "not a relay"))?;
    if !path_ok(kind, &job.path) { return Err(Error::status(403, "That address is not relayed.")); }
    let base = if p.base_url.is_empty() { r.base.to_string() } else { p.base_url.trim_end_matches('/').to_string() }; // baseUrl: tests (a fake provider)
    let body = job.body_bytes()?;
    let send = |t: Login| {
        let mut hm = reqwest::header::HeaderMap::new();
        for (k, v) in &job.headers { if pass_req(k) { if let (Ok(n), Some(Ok(val))) = (reqwest::header::HeaderName::from_bytes(k.to_ascii_lowercase().as_bytes()), v.as_str().map(reqwest::header::HeaderValue::from_str)) { hm.insert(n, val); } } }
        if kind == "claude-relay" {
            let mut betas: Vec<String> = hm.get("anthropic-beta").and_then(|v| v.to_str().ok()).unwrap_or("").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if !betas.iter().any(|b| b == "oauth-2025-04-20") { betas.push("oauth-2025-04-20".into()); }
            hm.insert("anthropic-beta", reqwest::header::HeaderValue::from_str(&betas.join(",")).expect("ascii"));
            if !hm.contains_key("anthropic-version") { hm.insert("anthropic-version", reqwest::header::HeaderValue::from_static("2023-06-01")); }
            hm.insert("authorization", reqwest::header::HeaderValue::from_str(&format!("Bearer {}", t.token)).unwrap_or(reqwest::header::HeaderValue::from_static("")));
        } else {
            let tok = if t.token.is_empty() { t.api_key.clone() } else { t.token.clone() };
            hm.insert("authorization", reqwest::header::HeaderValue::from_str(&format!("Bearer {tok}")).unwrap_or(reqwest::header::HeaderValue::from_static("")));
            if !t.account.is_empty() { if let Ok(v) = reqwest::header::HeaderValue::from_str(&t.account) { hm.insert("chatgpt-account-id", v); } }
        }
        let method = reqwest::Method::from_bytes(if job.method.is_empty() { b"POST" } else { job.method.as_bytes() }).unwrap_or(reqwest::Method::POST);
        h.http.request(method, format!("{base}{}", job.path)).headers(hm).body(body.clone()).timeout(Duration::from_secs(600)).send()
    };
    let mut t = login_token(p)?;
    if kind == "claude-relay" && t.expires_at > 0 && t.expires_at < now_ms() + 120_000 { refresh_login(h, p).await; t = login_token(p)?; }
    if t.token.is_empty() && t.api_key.is_empty() { return Err(Error::status(503, format!("This host has no {} login (sign in to it on this machine once).", r.name))); }
    let res = send(t).await?;
    if res.status().as_u16() == 401 { refresh_login(h, p).await; return Ok(send(login_token(p)?).await?); }
    Ok(res)
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_the_fixed_paths() {
        assert!(super::path_ok("claude-relay", "/v1/messages"));
        assert!(super::path_ok("claude-relay", "/v1/messages/count_tokens?beta=true"));
        assert!(!super::path_ok("claude-relay", "/v1/files"));
        assert!(!super::path_ok("claude-relay", "/v1/messages?x=<"));
        assert!(super::path_ok("codex-relay", "/responses"));
        assert!(!super::path_ok("codex-relay", "/v1/messages"));
        assert!(!super::path_ok("other", "/responses"));
    }
}
