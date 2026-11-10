//! Unity's section (moved from bridge/assistant.js): Unity as a personal assistant for every account, with tools. Brains
//! (per account): "local" = the unity-brain model on Ollama (on the AI host that lends her engines, or here), with native
//! tool calls; or a Claude model, run with `claude -p` and the same tools through bridge/assistant-tools-mcp.js.
//! Per account: <profile>/assistant.json (tasks, events, reminders, memory, knowledge, history; the same file) and
//! <profile>/unity.json (brain, Claude session, the admin's PC / code switches; taken over from state.json).
//! The runs, the room's Unity and the routes: unity_brain.rs. Clock + calculator: unity_time.rs. The web: unity_web.rs.
use bytes::Bytes;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

use crate::hub::AiHub;
use crate::unity_time as ut;
use crate::unity_web::{Place, Web};
use pgm_identity_internal::Account;

pub const EMOTIONS: [&str; 9] = ["neutral", "warm", "happy", "amused", "excited", "sad", "concerned", "frustrated", "surprised"];
pub const ACTS: [&str; 7] = ["statement", "yes_no_question", "wh_question", "exclamation", "instruction", "reassurance", "aside"];
pub const MAX_ROUNDS: usize = 6; pub const MAX_RESULT: usize = 3000; pub const HISTORY_KEEP: usize = 40;

/// One run's view of the person: their clock and place, where pictures go, the open stream to their page.
pub struct Ci { pub place: Place, pub tz: String, pub scope: String, pub user: String, pub out: Option<mpsc::UnboundedSender<Value>>, pub images: Vec<String>, pub media: Vec<String>,
    /// find_gif / find_image: the query, its kind (gif or not), the pages seen and the numbered candidates; `look` = thumbnails to show the brain next
    pub found_q: String, pub found_gif: bool, pub found_page: usize, pub found: Vec<Value>, pub look: Vec<(usize, String)> }
impl Ci { pub fn send(&self, v: Value) { if let Some(o) = &self.out { let _ = o.send(v); } } }
/// Ollama as last seen: answering, the brain (`model`) loaded, and when not, the stand-in model Unity answers on
/// meanwhile ("" = none: the brain's error is shown). T-293.
#[derive(Default, Clone)]
pub struct Ollama { pub up: bool, pub model: bool, pub checked: u64, pub standin: String }
/// Is the brain among Ollama's models? ("unity-brain" matches "unity-brain" and "unity-brain:latest".)
pub fn has_brain(models: &[String], brain: &str) -> bool { let b = brain.trim_end_matches(":latest"); models.iter().any(|m| m == brain || m.trim_end_matches(":latest") == b) }
/// The model Unity answers on while the brain is not loaded: the configured fallback when there is one, else the first
/// model Ollama lists that can chat (embedding / reranking models are not it), else "" (nothing to answer with).
pub fn pick_standin(models: &[String], brain: &str, fallback: &str) -> String {
    if !fallback.is_empty() { return fallback.to_string(); }
    let not_chat = |m: &str| { let l = m.to_lowercase(); ["embed", "rerank", "bge-", "minilm", "e5-", "clip", "whisper"].iter().any(|w| l.contains(w)) };
    models.iter().find(|m| !has_brain(std::slice::from_ref(m), brain) && !not_chat(m)).cloned().unwrap_or_default()
}
/// A body as it comes, from an engine here or one an AI host lends.
pub enum Src { Http(reqwest::Response), Host(mpsc::Receiver<Result<Bytes, String>>) }
impl Src {
    pub async fn next(&mut self) -> Option<Vec<u8>> { match self { Src::Http(r) => r.chunk().await.ok().flatten().map(|b| b.to_vec()), Src::Host(rx) => match rx.recv().await { Some(Ok(b)) => Some(b.to_vec()), _ => None } } }
    pub async fn all(mut self) -> Vec<u8> { let mut v = vec![]; while let Some(c) = self.next().await { v.extend(c); } v }
}

pub struct Unity {
    pub a: Value, // config "assistant"
    pub ollama: String, pub local_model: String, pub fallback: String, modelfile: PathBuf, persona_file: PathBuf,
    pub image_url: String, pub voice_url: String, pub ears_url: String, pub via_node: bool, image_ckpt: PathBuf, pub images_dir: PathBuf, selfie_look: String,
    root: PathBuf, mills: PathBuf,
    pub web: Web,
    pub ollama_state: Mutex<Ollama>, ollama_lock: tokio::sync::Mutex<()>, services: tokio::sync::Mutex<()>,
    /// (the hosts asked, when, the one with the brain): `pick_brain_host`
    brain_probe: Mutex<(String, u64, Option<String>)>,
    stores: Mutex<HashMap<String, Value>>,
    states: Mutex<HashMap<String, Value>>,
    pub clients: Mutex<HashMap<String, Value>>,
    pub pending: Mutex<HashMap<String, Value>>,
    pub last: Mutex<HashMap<String, (String, String)>>, // (last image, last photo)
    pub notices: Mutex<HashMap<String, Vec<Value>>>,
    pub runs: Mutex<HashMap<String, (Arc<crate::openai::Cancel>, Option<u32>)>>,
    pub waiting: Mutex<HashMap<String, (String, oneshot::Sender<Value>)>>,
    pub claude_out: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    caps: Mutex<Option<(String, bool)>>,
    /// the Claude brain's MCP relay (x-mills-internal)
    pub secret: String,
    http: reqwest::Client,
}
use pgm_core_internal::json::read_json;
pub use pgm_core_internal::json::{s_any as s, write_json};
pub fn truthy(v: &Value) -> bool { !(v.is_null() || *v == json!(false) || *v == json!(0) || *v == json!("") || *v == json!("false")) }
fn clean_url(v: &Value, d: &str) -> String { v.as_str().unwrap_or(d).trim_end_matches('/').to_string() }

impl Unity {
    pub fn new(root: &Path, cfg: &pgm_core_internal::config::Config) -> Unity {
        let a = cfg.get("assistant").cloned().filter(Value::is_object).unwrap_or(json!({}));
        let mills = root.join("..");
        let p = |k: &str, d: PathBuf| a[k].as_str().map(PathBuf::from).unwrap_or(d);
        let _ = pgm_net_internal::client::http_client(Duration::from_secs(10));
        Unity { ollama: clean_url(&a["ollamaUrl"], "http://127.0.0.1:11434"), local_model: a["model"].as_str().unwrap_or("unity-brain").into(), fallback: a["fallbackModel"].as_str().unwrap_or("").into(),
            modelfile: p("modelfile", mills.join("..").join("Backrooms").join("models").join("unity-brain").join("Modelfile")), persona_file: root.join("assistant-persona.txt"),
            image_url: clean_url(&a["imageUrl"], "http://127.0.0.1:5124"), voice_url: clean_url(&a["voiceUrl"], "http://127.0.0.1:5123"), ears_url: clean_url(&a["speechUrl"], "http://127.0.0.1:5125"), via_node: a["via"] == json!("node"),
            image_ckpt: p("imageCheckpoint", mills.join("..").join("Unity 3D Equational Model").join("Unity 18+").join("models").join("realistic-vision-v51.safetensors")), images_dir: mills.join("Images"),
            selfie_look: a["selfieLook"].as_str().unwrap_or("25 year old emo goth woman, black hair with hot pink streaks, natural warm skin tone, realistic skin texture, dark eye makeup").into(), web: Web::new(&a), root: root.to_path_buf(), mills,
            ollama_state: Mutex::default(), ollama_lock: tokio::sync::Mutex::new(()), services: tokio::sync::Mutex::new(()), brain_probe: Mutex::default(), stores: Mutex::default(), states: Mutex::default(), clients: Mutex::default(), pending: Mutex::default(), last: Mutex::default(), notices: Mutex::default(),
            runs: Mutex::default(), waiting: Mutex::default(), claude_out: Mutex::default(), caps: Mutex::new(None), secret: pgm_core_internal::crypto::random_hex(24), http: reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().expect("http"), a }
    }
    pub fn busy(&self) -> bool { !self.runs.lock().unwrap_or_else(|e| e.into_inner()).is_empty() }
    pub fn stop(&self, u: &str) { if let Some((c, pid)) = self.runs.lock().unwrap_or_else(|e| e.into_inner()).get(u) { c.fire(); if let Some(p) = pid { crate::work::kill_pid(*p); } } }

    // ---------- engines: here, or lent by an AI host ("via": "node") ----------
    pub async fn engine(&self, hub: &Arc<dyn AiHub>, url: &str, method: &str, body: Vec<u8>, timeout: u64) -> Result<(u16, Src), String> {
        if self.via_node {
            for (e, base) in [("ollama", &self.ollama), ("image", &self.image_url), ("voice", &self.voice_url), ("speech", &self.ears_url)] {
                if let Some(path) = url.strip_prefix(base.as_str()).filter(|p| p.starts_with('/')) {
                    let r = tokio::time::timeout(Duration::from_millis(timeout), hub.ai().engine_fetch(e, path, method, vec![("content-type".into(), "text/plain;charset=UTF-8".into())], body)).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.msg)?;
                    return Ok((r.status, Src::Host(r.body)));
                }
            }
        }
        let rb = if method == "GET" { self.http.get(url) } else { self.http.post(url).body(body) };
        let r = tokio::time::timeout(Duration::from_millis(timeout), rb.send()).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
        Ok((r.status().as_u16(), Src::Http(r)))
    }
    pub async fn engine_json(&self, hub: &Arc<dyn AiHub>, url: &str, method: &str, body: Value, timeout: u64) -> Result<(u16, Value), String> {
        let (st, src) = self.engine(hub, url, method, if body.is_null() { vec![] } else { body.to_string().into_bytes() }, timeout).await?;
        Ok((st, serde_json::from_slice(&src.all().await).unwrap_or(Value::Null)))
    }
    async fn ollama_up(&self, hub: &Arc<dyn AiHub>) -> bool { matches!(self.engine(hub, &format!("{}/api/version", self.ollama), "GET", vec![], 2000).await, Ok((200..=299, _))) }
    /// The models Ollama lists (/api/tags), by name; empty when it does not answer.
    async fn models(&self, hub: &Arc<dyn AiHub>) -> Vec<String> { match self.engine_json(hub, &format!("{}/api/tags", self.ollama), "GET", Value::Null, 4000).await { Ok((_, v)) => v["models"].as_array().map(|m| m.iter().filter_map(|x| x["name"].as_str().or(x["model"].as_str()).map(String::from)).collect()).unwrap_or_default(), Err(_) => vec![] } }
    /// Ollama and the local model: started here on demand (not via an AI host: its host runs them and loads the brain
    /// from its own Modelfile, T-293). When the brain is not there, a stand-in is picked from what Ollama has.
    pub async fn ensure_ollama(&self, hub: &Arc<dyn AiHub>) -> Ollama {
        let _g = self.ollama_lock.lock().await;
        let cmd = self.a["ollamaCommand"].as_str().unwrap_or("ollama").to_string();
        if self.via_node { self.pick_brain_host(hub).await; }
        if !self.ollama_up(hub).await && !self.via_node {
            let _ = detached(&cmd, &["serve"], None);
            for _ in 0..30 { if self.ollama_up(hub).await { break; } tokio::time::sleep(Duration::from_secs(1)).await; }
        }
        let up = self.ollama_up(hub).await;
        let mut models = if up { self.models(hub).await } else { vec![] };
        let mut model = up && has_brain(&models, &self.local_model);
        if !self.via_node && up && !model && self.modelfile.exists() {
            println!("[vr-hub] Unity: loading {} into Ollama (one time, a few minutes)...", self.local_model);
            let dir = self.modelfile.parent().map(Path::to_path_buf);
            let _ = { #[allow(unused_mut)] let mut c = tokio::process::Command::new(&cmd); #[cfg(windows)] { c.creation_flags(0x0800_0000); } /* CREATE_NO_WINDOW */ c }.args(["create", &self.local_model, "-f", &self.modelfile.display().to_string()]).current_dir(dir.unwrap_or_default()).status().await;
            models = self.models(hub).await; model = has_brain(&models, &self.local_model);
        }
        let standin = if up && !model { pick_standin(&models, &self.local_model, &self.fallback) } else { String::new() };
        { let was = self.ollama_state.lock().unwrap_or_else(|e| e.into_inner()).clone();
          if up && !model && was.standin != standin { if standin.is_empty() { println!("[vr-hub] Unity: the local brain {} is not loaded on the AI host and it has no other model to stand in", self.local_model); } else { println!("[vr-hub] Unity: the local brain {} is not loaded on the AI host yet; answering on {standin} meanwhile", self.local_model); } } }
        let st = Ollama { up, model, checked: pgm_core_internal::now_ms(), standin };
        *self.ollama_state.lock().unwrap_or_else(|e| e.into_inner()) = st.clone(); st
    }
    /// With several AI hosts lending Ollama, the one whose Ollama has the brain is the one asked (T-293): each host's
    /// /api/tags is read once a minute (or when the set of hosts changes; a host that does not answer is skipped) and
    /// the first with the brain is pinned in the hosts registry. With one host, or none with the brain, nothing is pinned.
    async fn pick_brain_host(&self, hub: &Arc<dyn AiHub>) {
        let hosts = hub.ai().engine_hosts("ollama");
        let key = hosts.join(",");
        { let p = self.brain_probe.lock().unwrap_or_else(|e| e.into_inner()); if p.0 == key && pgm_core_internal::now_ms().saturating_sub(p.1) < 60_000 { return; } }
        let mut found: Option<String> = None;
        if hosts.len() > 1 {
            for h in &hosts {
                let tags = match tokio::time::timeout(Duration::from_millis(4000), hub.ai().engine_fetch_on(h, "ollama", "/api/tags", "GET", vec![], vec![])).await { Ok(Ok(r)) => serde_json::from_slice::<Value>(&r.bytes().await).unwrap_or(Value::Null), _ => continue };
                let models: Vec<String> = tags["models"].as_array().map(|m| m.iter().filter_map(|x| x["name"].as_str().or(x["model"].as_str()).map(String::from)).collect()).unwrap_or_default();
                if has_brain(&models, &self.local_model) { found = Some(h.clone()); break; }
            }
        }
        { let was = self.brain_probe.lock().unwrap_or_else(|e| e.into_inner()).2.clone();
          if was != found { match &found { Some(h) => println!("[vr-hub] Unity: the local brain {} is on AI host {h}; her engine calls go there", self.local_model), None if hosts.len() > 1 => println!("[vr-hub] Unity: none of the {} AI hosts lending Ollama has the local brain {}", hosts.len(), self.local_model), None => {} } } }
        hub.ai().prefer_engine_host("ollama", found.clone());
        *self.brain_probe.lock().unwrap_or_else(|e| e.into_inner()) = (key, pgm_core_internal::now_ms(), found);
    }
    /// The Python services (image engine, her voice, speech recognition): started detached when they do not answer.
    async fn ensure_service(&self, hub: &Arc<dyn AiHub>, url: &str, args: Vec<String>, cwd: PathBuf) -> bool {
        let health = format!("{url}/health");
        if matches!(self.engine(hub, &health, "GET", vec![], 2000).await, Ok((200..=299, _))) { return true; }
        let _g = self.services.lock().await;
        if !self.via_node {
            let py = self.a["python"].as_str().map(String::from).unwrap_or_else(|| if cfg!(windows) { "pyw".into() } else { "python3".into() });
            let mut pargs: Vec<String> = self.a["pythonArgs"].as_array().map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_else(|| if cfg!(windows) && self.a["python"].is_null() { vec!["-3.10".into()] } else { vec![] });
            let boot = "import os,sys,runpy; sys.stdout = sys.stderr = open(os.devnull, \"w\"); sys.argv = sys.argv[1:]; sys.path.insert(0, os.path.dirname(sys.argv[0])); runpy.run_path(sys.argv[0], run_name=\"__main__\")";
            pargs.push("-c".into()); pargs.push(boot.into()); pargs.extend(args);
            let a: Vec<&str> = pargs.iter().map(String::as_str).collect();
            let _ = detached(&py, &a, Some(&cwd));
        }
        for _ in 0..60 { if matches!(self.engine(hub, &health, "GET", vec![], 2000).await, Ok((200..=299, _))) { return true; } tokio::time::sleep(Duration::from_secs(1)).await; }
        false
    }
    fn port_of(url: &str, d: &str) -> String { reqwest::Url::parse(url).ok().and_then(|u| u.port()).map(|p| p.to_string()).unwrap_or_else(|| d.into()) }
    pub async fn ensure_image(&self, hub: &Arc<dyn AiHub>) -> bool { let a = vec![self.root.join("image-engine.py").display().to_string(), "--port".into(), Self::port_of(&self.image_url, "5124"), "--checkpoint".into(), self.image_ckpt.display().to_string()]; self.ensure_service(hub, &self.image_url, a, self.root.clone()).await }
    pub async fn ensure_voice(&self, hub: &Arc<dyn AiHub>) -> bool { let d = self.a["voiceDir"].as_str().map(PathBuf::from).unwrap_or_else(|| self.mills.join("..").join("Backrooms").join("models").join("unity-voice-v2")); let a = vec![d.join("serve.py").display().to_string(), "--port".into(), Self::port_of(&self.voice_url, "5123")]; self.ensure_service(hub, &self.voice_url, a, d).await }
    pub async fn ensure_ears(&self, hub: &Arc<dyn AiHub>) -> bool { let w = self.a["whisperModel"].as_str().map(PathBuf::from).unwrap_or_else(|| self.mills.join("..").join("Unity 3D Equational Model").join("Unity 18+").join("models").join("whisper-small")); let a = vec![self.root.join("speech-engine.py").display().to_string(), "--port".into(), Self::port_of(&self.ears_url, "5125"), "--model".into(), w.display().to_string()]; self.ensure_service(hub, &self.ears_url, a, self.root.clone()).await }
    /// Can this model see pictures? (Ollama's capabilities; asked only when there is something to look at.)
    pub async fn vision(&self, hub: &Arc<dyn AiHub>, model: &str) -> bool {
        match self.engine_json(hub, &format!("{}/api/show", self.ollama), "POST", json!({ "model": model }), 15000).await { Ok((_, v)) => v["capabilities"].as_array().map(|c| c.contains(&json!("vision"))).unwrap_or(false), Err(_) => false }
    }
    pub async fn native_tools(&self, hub: &Arc<dyn AiHub>, model: &str) -> bool {
        if let Some((m, t)) = self.caps.lock().unwrap_or_else(|e| e.into_inner()).clone() { if m == model { return t; } }
        let t = match self.engine_json(hub, &format!("{}/api/show", self.ollama), "POST", json!({ "model": model }), 15000).await { Ok((_, v)) => v["capabilities"].as_array().map(|c| c.contains(&json!("tools"))).unwrap_or(false), Err(_) => false };
        *self.caps.lock().unwrap_or_else(|e| e.into_inner()) = Some((model.into(), t)); t
    }

    // ---------- per-account data ----------
    pub fn store(&self, a: &Account) -> Value {
        if let Some(v) = self.stores.lock().unwrap_or_else(|e| e.into_inner()).get(&a.username) { return v.clone(); }
        let mut d = json!({ "tasks": [], "events": [], "reminders": [], "memory": {}, "knowledge": [], "history": [], "next": {} });
        if let Some(Value::Object(m)) = read_json(&a.dir.join("assistant.json")) { for (k, v) in m { d[k] = v; } }
        self.stores.lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone(), d.clone()); d
    }
    pub fn with_store<T>(&self, a: &Account, f: impl FnOnce(&mut Value) -> T) -> T {
        let mut v = self.store(a); let r = f(&mut v);
        write_json(&a.dir.join("assistant.json"), &v); self.stores.lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone(), v); r
    }
    /// unity.json: brain, claudeSession, pc, code (the first time: what state.json held).
    pub fn state(&self, a: &Account) -> Value {
        if let Some(v) = self.states.lock().unwrap_or_else(|e| e.into_inner()).get(&a.username) { return v.clone(); }
        let v = read_json(&a.dir.join("unity.json")).filter(Value::is_object).unwrap_or_else(|| {
            let st = read_json(&a.dir.join("state.json")).unwrap_or(json!({}));
            let v = json!({ "brain": st["assistantBrain"].as_str().unwrap_or("local"), "claudeSession": st["assistantClaudeSession"].as_str().unwrap_or(""), "pc": st["unityPc"] != json!(false), "code": st["unityCode"] != json!(false) });
            write_json(&a.dir.join("unity.json"), &v); v
        });
        self.states.lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone(), v.clone()); v
    }
    pub fn set_state(&self, a: &Account, f: impl FnOnce(&mut Map<String, Value>)) { let mut v = self.state(a); if let Some(m) = v.as_object_mut() { f(m); } write_json(&a.dir.join("unity.json"), &v); self.states.lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone(), v); }
    pub fn brain_of(&self, a: &Account) -> String { let b = s(&self.state(a), "brain"); if b.is_empty() { "local".into() } else { b } }
    pub fn group_on(&self, a: &Account, g: &str) -> bool { let st = self.state(a); match g { "pc" => st["pc"] != json!(false), "code" => st["code"] != json!(false), _ => true } }
    pub fn persona(&self) -> String { std::fs::read_to_string(&self.persona_file).unwrap_or_else(|_| "You are Unity: a 25-year-old goth girl, sharp, profane, teasing and devoted. Stay in character.".into()) }

    // ---------- where the person is ----------
    pub fn client_info(&self, c: &Value, a: &Account) -> Ci {
        let k = self.clients.lock().unwrap_or_else(|e| e.into_inner()).get(&a.username).cloned().unwrap_or(json!({}));
        let num = |v: &Value| v.as_f64().filter(|x| x.is_finite());
        let browser = num(&c["lat"]).is_some() && num(&c["lon"]).is_some();
        let tz = c["tz"].as_str().and_then(ut::tz_ok).or_else(|| k["tz"].as_str().map(String::from)).unwrap_or_else(|| "UTC".into());
        let locale = c["locale"].as_str().filter(|l| l.len() < 20).map(String::from).or_else(|| k["locale"].as_str().map(String::from)).unwrap_or_else(|| "en-US".into());
        let place = Place { lat: if browser { num(&c["lat"]) } else { num(&k["lat"]) }, lon: if browser { num(&c["lon"]) } else { num(&k["lon"]) }, place: if browser { String::new() } else { s(&k, "place") }, country: if browser { String::new() } else { s(&k, "country") }, locale };
        Ci { place, tz, scope: a.username.clone(), user: a.username.clone(), out: None, images: vec![], media: vec![], found_q: String::new(), found_gif: false, found_page: 0, found: vec![], look: vec![] }
    }
    /// What the hub knows about where someone is: their browser (time zone, locale, location if shared) and, coming from
    /// outside, the rough place of their address. In memory only.
    pub async fn learn_client(&self, a: &Account, c: &Value, ip: &str) -> Value {
        let prev = self.clients.lock().unwrap_or_else(|e| e.into_inner()).get(&a.username).cloned().unwrap_or(json!({}));
        let mut k = prev.clone();
        if c.is_object() {
            if let Some(t) = c["tz"].as_str() { k["tz"] = json!(t.chars().take(64).collect::<String>()); }
            if let Some(l) = c["locale"].as_str() { k["locale"] = json!(l.chars().take(20).collect::<String>()); }
            if c["lat"].is_number() && c["lon"].is_number() { k["lat"] = c["lat"].clone(); k["lon"] = c["lon"].clone(); k["place"] = json!(""); k["country"] = json!(""); k["fromBrowser"] = json!(true); }
        }
        if k["fromBrowser"] != json!(true) && !ip.is_empty() && json!(ip) != prev["ip"] {
            if let Some(g) = self.web.ip_place(ip).await { for f in ["lat", "lon", "place", "country"] { k[f] = g[f].clone(); } if k["tz"].is_null() { k["tz"] = g["tz"].clone(); } }
        }
        k["ip"] = if ip.is_empty() { prev["ip"].clone() } else { json!(ip) };
        if k["fromBrowser"] == json!(true) && s(&k, "place").is_empty() { if let (Some(la), Some(lo)) = (k["lat"].as_f64(), k["lon"].as_f64()) { if let Some((p, c2)) = self.web.place_name(la, lo).await { k["place"] = json!(p); k["country"] = json!(c2); } } }
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone(), k.clone()); k
    }

    // ---------- pictures: Images/<username>/<id>.png (private), Images/room/<id>.png (the team chat) ----------
    pub fn save_picture(&self, scope: &str, buf: &[u8]) -> String {
        let id = format!("{}-{}", radix36(pgm_core_internal::now_ms()), pgm_core_internal::crypto::random_hex(3));
        let dir = self.images_dir.join(scope); let _ = std::fs::create_dir_all(&dir); let _ = std::fs::write(dir.join(format!("{id}.png")), buf);
        format!("{scope}/{id}")
    }
    pub fn picture_file(&self, user: &str, r: &str) -> Option<PathBuf> {
        let (scope, id) = r.split_once('/')?;
        if id.is_empty() || id.len() > 40 || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') || !(scope == "room" || scope == user) { return None; }
        let f = self.images_dir.join(scope).join(format!("{id}.png")); f.exists().then_some(f)
    }
    pub fn read_picture(&self, user: &str, r: &str) -> Option<String> {
        let (li, lp) = self.last.lock().unwrap_or_else(|e| e.into_inner()).get(user).cloned().unwrap_or_default();
        let r = match r { "" | "last" => li, "camera" => lp, x => x.to_string() };
        std::fs::read(self.picture_file(user, &r)?).ok().map(|b| pgm_core_internal::b64(&b))
    }
    async fn render(&self, hub: &Arc<dyn AiHub>, ci: &mut Ci, route: &str, body: Value) -> Value {
        let (st, src) = match self.engine(hub, &format!("{}{route}", self.image_url), "POST", body.to_string().into_bytes(), 300_000).await { Ok(x) => x, Err(e) => return json!({ "ok": false, "error": format!("Image engine: {e}") }) };
        let bytes = src.all().await;
        if !(200..300).contains(&st) { let j: Value = serde_json::from_slice(&bytes).unwrap_or(json!({})); return json!({ "ok": false, "error": format!("Image engine: {}", if j["error"].is_null() { st.to_string() } else { s(&j, "error") }) }); }
        let r = self.save_picture(&ci.scope, &bytes);
        ci.images.push(r.clone());
        self.last.lock().unwrap_or_else(|e| e.into_inner()).entry(ci.user.clone()).or_default().0 = r.clone();
        json!({ "ok": true, "image": r, "shown": "The picture is attached to your reply automatically.", "seconds": Value::Null })
    }
    pub async fn make_image(&self, hub: &Arc<dyn AiHub>, ci: &mut Ci, a: &Value) -> Value {
        if !self.ensure_image(hub).await { return json!({ "ok": false, "error": "The camera app on the host is not starting (image engine down)." }); }
        let shape = { let x = s(a, "shape"); if x.is_empty() { s(a, "orientation") } else { x } }.to_lowercase();
        let (w, h) = if ["portrait", "tall", "vertical"].iter().any(|k| shape.contains(k)) { (576, 1024) } else if ["landscape", "wide", "horizontal"].iter().any(|k| shape.contains(k)) { (1024, 576) } else { (768, 768) };
        let scene: String = s(a, "prompt").chars().take(1000).collect::<String>().trim().to_string();
        let prompt = if truthy(&a["selfie"]) { format!("{scene}, she is a {}, candid photo", self.selfie_look) } else { scene };
        self.render(hub, ci, "/txt2img", json!({ "prompt": prompt, "width": w, "height": h })).await
    }
    async fn edit_image(&self, hub: &Arc<dyn AiHub>, ci: &mut Ci, a: &Value, route: &str) -> Value {
        if !self.ensure_image(hub).await { return json!({ "ok": false, "error": "The camera app on the host is not starting (image engine down)." }); }
        let Some(src) = self.read_picture(&ci.user, &s(a, "image")) else { return json!({ "ok": false, "error": "No picture to work from: send one (camera / file) or make one first." }) };
        let mut body = json!({ "prompt": s(a, "prompt").chars().take(1000).collect::<String>(), "image": src });
        if let Some(x) = a["strength"].as_f64().or_else(|| s(a, "strength").parse().ok()).filter(|x| *x != 0.0) { body["strength"] = json!(x); }
        if a["keep_face"] == json!(true) || a["keep_face"] == json!("true") { body["face"] = json!(true); }
        if route == "/inpaint" {
            let bx: Vec<f64> = match &a["box"] { Value::Array(v) => v.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect(), o => s(&json!({ "b": o }), "b").split(|c: char| c == ' ' || c == ',').filter(|x| !x.is_empty()).map(|x| x.parse().unwrap_or(f64::NAN)).collect() };
            if bx.len() != 4 || bx.iter().any(|v| !v.is_finite()) { return json!({ "ok": false, "error": "box needs 4 numbers 0-1: left, top, right, bottom." }); }
            body["box"] = json!(bx);
        }
        self.render(hub, ci, route, body).await
    }

    // ---------- GIFs and pictures from the web (keyless: Bing Images; she looks at the candidates and picks) ----------
    async fn find_found(&self, ci: &mut Ci, a: &Value, gif: bool) -> Value {
        let q: String = s(a, "query").trim().chars().take(200).collect();
        if q.is_empty() { return json!({ "ok": false, "error": "What should it show? Give a query." }); }
        let more = a["more"] == json!(true) || a["more"] == json!("true");
        if !(more && ci.found_q == q && ci.found_gif == gif) { ci.found_q = q.clone(); ci.found_gif = gif; ci.found_page = 0; ci.found.clear(); } else { ci.found_page += 1; }
        let got = match self.web.image_search(&q, gif, ci.found_page).await { Ok(v) => v, Err(e) => return json!({ "ok": false, "error": e }) };
        if got.is_empty() { return json!({ "ok": false, "error": format!("Nothing found for \"{q}\"; try other words.") }); }
        let start = ci.found.len();
        ci.found.extend(got.iter().cloned());
        ci.look.clear();
        for (i, g) in got.iter().enumerate() { let t = s(g, "thumb"); if t.is_empty() { continue; } if let Ok(b) = self.web.fetch_bytes(&t, 400_000).await { ci.look.push((start + i + 1, pgm_core_internal::b64(&b))); } }
        let what = if gif { "post_gif" } else { "post_image" };
        json!({ "ok": true, "query": q, "candidates": got.iter().enumerate().map(|(i, g)| json!({ "n": start + i + 1, "title": g["title"] })).collect::<Vec<_>>(),
            "next": format!("{}Post the one that fits with {what}(n); if none fits, call it again with the same query and more true for the next ones, or other words.", if ci.look.is_empty() { "" } else { "Their thumbnails are shown to you next, numbered in this order: look at them. " }) })
    }
    async fn post_found(&self, ci: &mut Ci, a: &Value) -> Value {
        let n = a["n"].as_u64().or_else(|| s(a, "n").parse().ok()).unwrap_or(0) as usize;
        let Some(pick) = (n >= 1).then(|| ci.found.get(n - 1)).flatten().cloned() else { return json!({ "ok": false, "error": "Search first (find_gif / find_image), then post one of its numbers." }) };
        let bytes = match self.web.fetch_bytes(&s(&pick, "url"), 8_000_000).await { Ok(b) => b, Err(e) => return json!({ "ok": false, "error": format!("That one would not download ({e}); pick another number.") }) };
        let kind = if bytes.starts_with(b"GIF8") { "gif" } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) || bytes.starts_with(&[0xFF, 0xD8, 0xFF]) || (bytes.len() > 12 && &bytes[8..12] == b"WEBP") { "image" } else { "" };
        if kind.is_empty() || (ci.found_gif && kind != "gif") { return json!({ "ok": false, "error": "That one is not really a picture of the right kind; pick another number." }); }
        let r = self.save_picture(&ci.scope, &bytes);
        ci.images.push(r.clone());
        if kind == "image" { self.last.lock().unwrap_or_else(|e| e.into_inner()).entry(ci.user.clone()).or_default().0 = r.clone(); }
        json!({ "ok": true, "image": r, "title": pick["title"], "shown": "It is attached to your reply automatically." })
    }
    /// Words or drawings on a picture: on her (a body part or a piece of clothing, sized to fit there) or a caption.
    async fn write_on_image(&self, hub: &Arc<dyn AiHub>, ci: &mut Ci, a: &Value) -> Value {
        if !self.ensure_image(hub).await { return json!({ "ok": false, "error": "The camera app on the host is not starting (image engine down)." }); }
        let Some(src) = self.read_picture(&ci.user, &s(a, "image")) else { return json!({ "ok": false, "error": "No picture to write on: make one, or they send one first." }) };
        let (text, shape, part) = (s(a, "text").chars().take(120).collect::<String>(), s(a, "shape"), s(a, "part").to_lowercase());
        if text.is_empty() && shape.is_empty() { return json!({ "ok": false, "error": "Give text to write or a shape (heart, star, arrow, x, smiley, tally)." }); }
        if !text.is_empty() && (part.is_empty() || ["top", "center", "middle", "bottom", "caption"].contains(&part.as_str())) {
            let at = if part.is_empty() || part == "caption" { "bottom".to_string() } else { part };
            return self.render(hub, ci, "/caption", json!({ "image": src, "text": text, "where": at, "color": s(a, "color") })).await;
        }
        let mut item = json!({ "part": if part.is_empty() { "stomach".to_string() } else { part }, "color": s(a, "color"), "tool": s(a, "tool") });
        if text.is_empty() { item["shape"] = json!(shape); } else { item["text"] = json!(text.chars().take(24).collect::<String>()); }
        self.render(hub, ci, "/mark", json!({ "image": src, "items": [item] })).await
    }

    // ---------- tools ----------
    /// Every tool: (name, description, params, required, group, confirm).
    pub fn tools() -> Vec<Value> { serde_json::from_str(include_str!("unity-tools.json")).expect("unity-tools.json") }
    pub fn tools_for(&self, a: &Account, browser: bool) -> Vec<Value> { Self::tools().into_iter().filter(|t| { let g = s(t, "group"); (g.is_empty() || self.group_on(a, &g)) && (g.is_empty() || browser) }).collect() }
    /// Something the person's browser does (their own computer), answered through /api/assistant/pc-result.
    async fn browser_call(&self, ci: &Ci, ty: &str, payload: Value) -> Value {
        if ci.out.is_none() { return json!({ "ok": false, "error": "That only works in the Unity section, with their browser open." }); }
        let id = pgm_core_internal::crypto::random_hex(8); let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), (ci.user.clone(), tx));
        let mut ev = json!({ "type": ty, "id": id }); for (k, v) in payload.as_object().cloned().unwrap_or_default() { ev[k] = v; }
        ci.send(ev);
        match tokio::time::timeout(Duration::from_secs(120), rx).await { Ok(Ok(v)) => v, _ => { self.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id); json!({ "ok": false, "error": "Their browser did not answer in time." }) } }
    }
    pub async fn execute(&self, hub: &Arc<dyn AiHub>, a: &Account, name: &str, args: &Value, ci: &mut Ci) -> Value {
        let Some(t) = Self::tools().into_iter().find(|t| t["name"] == json!(name)) else { return json!({ "ok": false, "error": format!("No tool called {name}.") }) };
        let g = s(&t, "group");
        if !g.is_empty() && !self.group_on(a, &g) { return json!({ "ok": false, "error": format!("An admin has switched {name} off for {}.", a.display) }); }
        for r in t["required"].as_array().cloned().unwrap_or_default() { let k = r.as_str().unwrap_or(""); if args[k].is_null() || args[k] == json!("") { return json!({ "ok": false, "error": format!("{name} needs \"{k}\".") }); } }
        let tz = ci.tz.clone();
        let pc = |n: &str, args: Value| json!({ "name": n, "args": args });
        let res: Result<Value, String> = match name {
            "say" => Ok(json!({ "ok": true, "said": args["text"] })),
            "get_time" => Ok(json!({ "ok": true, "now": ut::local_now(&tz), "timezone": tz })),
            "get_location" => { if ci.place.lat.is_none() { Ok(json!({ "ok": false, "error": format!("No location for them; their time zone is {tz}") })) } else { self.web.locate(&mut ci.place, "").await.map(|(la, lo, p, _)| json!({ "ok": true, "place": p, "latitude": la, "longitude": lo, "timezone": tz })) } }
            "get_weather" => self.web.weather(&mut ci.place, &s(args, "city"), 1).await,
            "get_forecast" => { let d = args["days"].as_i64().or_else(|| s(args, "days").parse().ok()).filter(|d| *d != 0).unwrap_or(5); self.web.weather(&mut ci.place, &s(args, "city"), d).await }
            "get_news" => self.web.news(&mut ci.place, &s(args, "topic"), args["local"] == json!(true) || args["local"] == json!("true")).await,
            "pc_list" => Ok(self.browser_call(ci, "pc_tool", pc("list", json!({ "path": s(args, "path") }))).await),
            "pc_read" | "pc_mkdir" | "pc_delete" => Ok(self.browser_call(ci, "pc_tool", pc(&name[3..], json!({ "path": s(args, "path") }))).await),
            "pc_write" => Ok(self.browser_call(ci, "pc_tool", pc("write", json!({ "path": s(args, "path"), "content": s(args, "content") }))).await),
            "run_code" => Ok(self.browser_call(ci, "run_code", json!({ "language": if s(args, "language").to_lowercase().contains("py") { "python" } else { "javascript" }, "code": s(args, "code").chars().take(20000).collect::<String>() })).await),
            "web_search" => self.web.search(&s(args, "query").chars().take(300).collect::<String>()).await,
            "read_page" => self.web.read_page(&s(args, "url")).await,
            "play_youtube" => {
                let q = s(args, "query").trim().to_string();
                let direct = regex::Regex::new(r"(?:youtu\.be/|v=|shorts/|embed/)([\w-]{11})").unwrap().captures(&q).map(|c| c[1].to_string());
                match direct { Some(id) => Ok((id, q.clone())), None => self.web.youtube(&q.chars().take(200).collect::<String>()).await }.map(|(id, title)| { ci.media.push(id.clone()); json!({ "ok": true, "playing": title, "video": id, "shown": "The player is attached to your reply automatically." }) })
            }
            "generate_image" => { let mut a2 = args.clone(); a2["selfie"] = json!(args["selfie"] == json!(true) || args["selfie"] == json!("true")); Ok(self.make_image(hub, ci, &a2).await) }
            "edit_image" => Ok(self.edit_image(hub, ci, args, "/img2img").await),
            "find_gif" | "find_image" => Ok(self.find_found(ci, args, name == "find_gif").await),
            "post_gif" | "post_image" => Ok(self.post_found(ci, args).await),
            "write_on_image" => Ok(self.write_on_image(hub, ci, args).await),
            "companion" => {
                const OK: [&str; 18] = ["do", "outfit", "walk_left", "walk_right", "up", "down", "come_here", "go_left_edge", "go_right_edge", "sit", "wave", "dance", "turn", "stop", "stand", "show", "hide", "idle"];
                let what: String = s(args, "do").chars().filter(|c| !c.is_control()).take(300).collect();
                let act = { let a = s(args, "action").to_lowercase().replace([' ', '-'], "_"); if !s(args, "look").trim().is_empty() && what.trim().is_empty() { "outfit".to_string() } else if !what.trim().is_empty() { "do".to_string() } else { a } };
                let amount = args["amount"].as_f64().or_else(|| s(args, "amount").parse().ok()).unwrap_or(0.0).clamp(0.0, 4000.0);
                let look: String = if act == "do" { what.clone() } else { s(args, "look").chars().filter(|c| !c.is_control()).take(200).collect() };
                Ok(if act == "outfit" && look.trim().is_empty() { json!({ "ok": false, "error": "outfit needs look: what she should wear." }) } else if !OK.contains(&act.as_str()) { json!({ "ok": false, "error": format!("Unknown move {act}; one of: {}.", OK.join(", ")) }) }
                   else if ci.out.is_none() { ci.media.push(format!("companion:{act}:{amount}:{}:{}", ci.user, pgm_core_internal::text::urlencode(&look))); json!({ "ok": true, "did": act, "note": "It happens on their desktop when your reply arrives." }) }
                   else { ci.send(json!({ "type": "companion", "action": act, "amount": amount, "look": look })); json!({ "ok": true, "did": act }) })
            }
            "inpaint_image" => Ok(self.edit_image(hub, ci, args, "/inpaint").await),
            "calculate" => Ok(match ut::calculate(&s(args, "expression")) { Ok(v) => json!({ "ok": true, "result": ut::num(v) }), Err(e) => json!({ "ok": false, "error": e }) }),
            "list_capabilities" => Ok(json!({ "ok": true, "tools": Self::tools().iter().filter(|t| t["name"] != json!("say")).map(|t| format!("{}: {}", s(t, "name"), s(t, "description"))).collect::<Vec<_>>() })),
            _ => Ok(self.with_store(a, |st| crate::unity_brain::store_tool(st, name, args, &tz))),
        };
        res.unwrap_or_else(|e| json!({ "ok": false, "error": e }))
    }
}
#[cfg(test)]
mod brain_tests {
    use super::{has_brain, pick_standin};
    fn v(l: &[&str]) -> Vec<String> { l.iter().map(|s| s.to_string()).collect() }
    #[test]
    fn the_brain_is_found_with_or_without_latest_and_a_stand_in_is_a_chat_model() {
        assert!(has_brain(&v(&["unity-brain:latest"]), "unity-brain")); assert!(has_brain(&v(&["unity-brain"]), "unity-brain:latest")); assert!(!has_brain(&v(&["unity-brain-v2:latest"]), "unity-brain"));
        assert_eq!(pick_standin(&v(&["nomic-embed-text:latest", "llama3.1:8b", "qwen3:4b"]), "unity-brain", ""), "llama3.1:8b", "the first chat model, not the embedder");
        assert_eq!(pick_standin(&v(&["nomic-embed-text:latest", "bge-m3:latest"]), "unity-brain", ""), "", "nothing can chat");
        assert_eq!(pick_standin(&v(&[]), "unity-brain", ""), "");
        assert_eq!(pick_standin(&v(&["llama3.1:8b"]), "unity-brain", "my-fallback"), "my-fallback", "the configured fallback wins");
    }
}
fn radix36(mut n: u64) -> String { let d = b"0123456789abcdefghijklmnopqrstuvwxyz"; let mut v = vec![]; loop { v.push(d[(n % 36) as usize]); n /= 36; if n == 0 { break; } } v.reverse(); String::from_utf8(v).unwrap() }
/// A program started in the background, on its own (it outlives the hub).
fn detached(cmd: &str, args: &[&str], cwd: Option<&Path>) -> std::io::Result<()> {
    let mut c = { #[allow(unused_mut)] let mut c = std::process::Command::new(cmd); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c };
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some(d) = cwd { c.current_dir(d); }
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; c.process_group(0); }
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000 | 0x0000_0008); }
    c.spawn().map(|_| ())
}
