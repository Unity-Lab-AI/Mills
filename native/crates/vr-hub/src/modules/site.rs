//! The page itself (moved from bridge/server.js): web/ as files, index.html with this hub's values filled in, every
//! script and stylesheet asked for under this build's name (no browser keeps an older copy), and no inline script
//! running but the page's own (its CSP's 'unsafe-inline' for scripts replaced by the sha256 of each inline script).
//! Also what a page learns at start (/api/check) and the build every signed-in answer names (X-VR-Version).
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use super::auth::Account;
use super::Routed;
use crate::front::Hub;

/// version + "+" + the commit's first 7, read once at start (a restart picks up a new one, as the pages expect).
pub struct Build { at: Mutex<(u64, String, Value)> }
impl Default for Build { fn default() -> Self { Build { at: Mutex::new((0, String::new(), json!({}))) } } }
impl Build {
    pub fn get(&self, hub: &Hub) -> (String, Value) {
        let mut b = self.at.lock().unwrap();
        if b.0 == 0 {
            let (v, c) = super::ai_hosts::release_now(&hub.root);
            let mods = std::fs::read_to_string(hub.root.join("..").join("versions.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| v["modules"].clone()).unwrap_or(json!({}));
            *b = (vr_core::now_ms(), if c.is_empty() { v } else { format!("{v}+{}", &c[..7.min(c.len())]) }, mods);
        }
        (b.1.clone(), b.2.clone())
    }
}
fn mime(ext: &str) -> &'static str { match ext { "html" => "text/html", "js" => "text/javascript", "css" => "text/css", "svg" => "image/svg+xml", "png" => "image/png", "webmanifest" => "application/manifest+json", "json" => "application/json", _ => "application/octet-stream" } }
fn public_url(hub: &Hub) -> String {
    let u = hub.cfg.get("publicUrl").and_then(Value::as_str).unwrap_or("").trim_end_matches('/').to_lowercase();
    if regex::Regex::new(r"^https://[a-z0-9.-]+(:\d+)?$").unwrap().is_match(&u) { u } else { String::new() }
}
/// GET of anything that is not /api/: web/<path> (index.html for /).
pub fn page(hub: &Arc<Hub>, path: &str) -> Routed {
    let web = hub.root.join("..").join("web");
    let rel = if path == "/" { "index.html".to_string() } else { path.trim_start_matches('/').to_string() };
    let nf = || Routed::Answer(crate::front::json(404, &json!({ "error": "Not found" })));
    // inside web/ only: no "..", no empty part, no drive letter or stream name (on Windows "C:…" would be a path of its own)
    if rel.contains(':') || rel.split(['/', '\\']).any(|c| c == ".." || c.is_empty()) { return nf(); }
    let f = web.join(&rel);
    if !f.is_file() { return nf(); }
    let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if name == "index.html" {
        let Ok(mut html) = std::fs::read_to_string(&f) else { return nf() };
        html = html.replace("__MILLS_SERVED__", "hub");
        let pu = public_url(hub); if !pu.is_empty() { html = html.replace("__MILLS_HUB_HOST__", &pu); }
        let build = vr_core::hub::urlencode(&hub.mods.build.get(hub).0);
        html = regex::Regex::new(r#"(<script src|<link rel="stylesheet" href)="([\w./-]+\.(?:js|css))""#).unwrap().replace_all(&html, |c: &regex::Captures| format!("{}=\"{}?v={build}\"", &c[1], &c[2])).to_string();
        // the page's own inline scripts only
        use base64::Engine;
        let hashes: Vec<String> = regex::Regex::new(r"(?s)<script>(.*?)</script>").unwrap().captures_iter(&html).map(|c| format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(vr_core::crypto::sha256(c[1].as_bytes())))).collect();
        html = regex::Regex::new(r#"(<meta http-equiv="Content-Security-Policy" content="[^"]*?script-src 'self') 'unsafe-inline';"#).unwrap().replacen(&html, 1, |c: &regex::Captures| format!("{} {}; worker-src blob:;", &c[1], hashes.join(" "))).to_string();
        return Routed::Answer(hyper::Response::builder().status(200).header("content-type", "text/html").header("cache-control", "no-cache, no-store, must-revalidate").body(crate::front::full(html)).expect("response"));
    }
    let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match std::fs::read(&f) { Ok(b) => Routed::Answer(hyper::Response::builder().status(200).header("content-type", mime(&ext)).header("cache-control", "no-cache").body(crate::front::full(b)).expect("response")), Err(_) => nf() }
}
/// /api/check: who this is, their choices, what there is.
pub fn check(hub: &Arc<Hub>, a: &Account) -> Value {
    let (w, ai) = (&hub.mods.work, &hub.mods.ai);
    let st = w.state(a); let prefs = std::fs::read_to_string(a.dir.join("state.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| v["prefs"].clone()).filter(Value::is_object).unwrap_or(json!({}));
    let (build, mods) = hub.mods.build.get(hub);
    let s = |k: &str| st[k].as_str().unwrap_or("").to_string();
    json!({ "ok": true, "username": a.username, "display": a.display, "admin": a.admin, "models": hub.cfg.get("models").cloned().unwrap_or(json!([])), "model": s("model"), "defaultModel": w.default_model_of(a),
        "project": w.project_of(a), "projects": w.project_list(), "prefs": prefs, "share": w.share_of(&a.username),
        "provider": if s("provider").is_empty() { "host".to_string() } else { s("provider") }, "effectiveProvider": w.effective(ai, &a.username, &s("provider")).map(|p| p["id"].clone()).unwrap_or(json!("")),
        "defaultProvider": w.default_for(ai, &a.username).map(|p| p["id"].clone()).unwrap_or(json!("")), "hubClaude": hub.cfg.get("hubClaude") != Some(&json!(false)),
        "providers": super::work::providers_list(hub, &a.username), "version": build.split('+').next().unwrap_or(""), "build": build, "versions": mods,
        "aiHost": !hub.root.join("ai-host.off").exists(), "aiNodes": ai.status(), "pcHost": ai.pc_host_for(&a.username) })
}
