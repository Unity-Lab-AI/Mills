//! The app's GPU switches for this computer's AI engines (tray + Tools window, Windows and Linux):
//! - free: the image engine drops its model (POST /unload, the process stays) and Ollama unloads every model it holds
//!   (keep_alive 0), so the GPU is free at once; the next request loads them again.
//! - image off: the image engine exits (POST /shutdown); pictures cannot be made until it is on again.
//! - image on: starts it again with the command in `<host folder>/engines-start.json`
//!   (`{ "image": { "program": "...", "args": ["..."], "cwd": "..." } }`), the same one the computer starts it with at sign-in.
//! The engines' addresses come from the AI host's own ai-node.json (`engines.image`, `engines.ollama`), else the defaults.
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

fn engines(dir: &str) -> (String, String) {
    let cfg: Value = std::fs::read_to_string(crate::aihost::config(dir)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    let local = |v: &Value, d: &str| { let u = v.as_str().unwrap_or(d).trim_end_matches('/').to_string(); if u.starts_with("http://127.0.0.1:") || u.starts_with("http://localhost:") { u } else { d.to_string() } };
    let ollama = std::env::var("OLLAMA_HOST").ok().filter(|h| !h.is_empty()).map(|h| if h.starts_with("http") { h } else { format!("http://{h}") }).map(|h| local(&json!(h), "http://127.0.0.1:11434"));
    (local(&cfg["engines"]["image"], "http://127.0.0.1:5124"), ollama.unwrap_or_else(|| local(&cfg["engines"]["ollama"], "http://127.0.0.1:11434")))
}
// the PGM client every native program uses (it installs the TLS provider first); local engines answer fast or not at all
fn client() -> reqwest::Client { pgm_net_internal::client::http_client(Duration::from_secs(3)).unwrap_or_default() }

/// What holds the GPU now: the image engine (up, model loaded) and Ollama's loaded models.
pub async fn status(dir: &str) -> Value {
    let (image, ollama) = engines(dir);
    let c = client();
    let img: Option<Value> = match c.get(format!("{image}/health")).send().await { Ok(r) => r.json().await.ok(), Err(_) => None };
    let ps: Option<Value> = match c.get(format!("{ollama}/api/ps")).send().await { Ok(r) => r.json().await.ok(), Err(_) => None };
    let models: Vec<Value> = ps.as_ref().and_then(|p| p["models"].as_array().cloned()).unwrap_or_default().iter().map(|m| json!({ "name": m["name"], "vram": m["size_vram"] })).collect();
    json!({ "image": { "up": img.is_some(), "loaded": img.as_ref().map(|h| h["loaded"] == json!(true)).unwrap_or(false), "canStart": start_cmd(dir).is_some() },
            "ollama": { "up": ps.is_some(), "models": models } })
}

/// Frees the GPU: the image engine's model and every model Ollama holds. Says what it freed.
pub async fn free(dir: &str) -> Result<String, String> {
    let (image, ollama) = engines(dir);
    let c = client();
    let mut done = vec![];
    if let Ok(r) = c.post(format!("{image}/unload")).body("{}").send().await { if r.status().is_success() { done.push("the image engine's model".to_string()); } }
    let ps: Value = match c.get(format!("{ollama}/api/ps")).send().await { Ok(r) => r.json().await.unwrap_or(Value::Null), Err(_) => Value::Null };
    for m in ps["models"].as_array().cloned().unwrap_or_default() {
        let Some(name) = m["name"].as_str() else { continue };
        if c.post(format!("{ollama}/api/generate")).json(&json!({ "model": name, "keep_alive": 0 })).send().await.map(|r| r.status().is_success()).unwrap_or(false) { done.push(name.to_string()); }
    }
    Ok(if done.is_empty() { "Nothing was holding the GPU.".into() } else { format!("Freed the GPU: {}.", done.join(", ")) })
}

fn start_cmd(dir: &str) -> Option<(String, Vec<String>, Option<PathBuf>)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(PathBuf::from(dir).join("engines-start.json")).ok()?).ok()?;
    let i = &v["image"];
    let program = i["program"].as_str().filter(|p| !p.is_empty())?.to_string();
    let args = i["args"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    Some((program, args, i["cwd"].as_str().filter(|c| !c.is_empty()).map(PathBuf::from)))
}

/// "on" | "off" for the image engine.
pub async fn image(dir: &str, on: bool) -> Result<String, String> {
    let (image, _) = engines(dir);
    let c = client();
    let up = c.get(format!("{image}/health")).send().await.map(|r| r.status().is_success()).unwrap_or(false);
    if !on {
        if !up { return Ok("The image engine is already off.".into()); }
        let _ = c.post(format!("{image}/shutdown")).body("{}").send().await;
        return Ok("The image engine is off: Unity cannot make pictures until it is on again (tray: Image engine: on).".into());
    }
    if up { return Ok("The image engine is already on.".into()); }
    let (program, args, cwd) = start_cmd(dir).ok_or("No start command for the image engine on this computer (engines-start.json in the AI host's folder).")?;
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some(d) = cwd { cmd.current_dir(d); }
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; cmd.creation_flags(0x0800_0000 | 0x0000_0008); } // no window, detached
    cmd.spawn().map_err(|e| format!("could not start the image engine: {e}"))?;
    for _ in 0..30 { tokio::time::sleep(Duration::from_secs(1)).await; if c.get(format!("{image}/health")).send().await.map(|r| r.status().is_success()).unwrap_or(false) { return Ok("The image engine is on.".into()); } }
    Err("The image engine was started but does not answer yet.".into())
}
