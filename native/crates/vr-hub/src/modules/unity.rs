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

use super::auth::Account;
use super::unity_time as ut;
use super::unity_web::{Place, Web};
use crate::front::Hub;

pub const EMOTIONS: [&str; 9] = ["neutral", "warm", "happy", "amused", "excited", "sad", "concerned", "frustrated", "surprised"];
pub const ACTS: [&str; 7] = ["statement", "yes_no_question", "wh_question", "exclamation", "instruction", "reassurance", "aside"];
pub const MAX_ROUNDS: usize = 6; pub const MAX_RESULT: usize = 3000; pub const HISTORY_KEEP: usize = 40;

/// One run's view of the person: their clock and place, where pictures go, the open stream to their page.
pub struct Ci { pub place: Place, pub tz: String, pub scope: String, pub user: String, pub out: Option<mpsc::UnboundedSender<Value>>, pub images: Vec<String>, pub media: Vec<String> }
impl Ci { pub fn send(&self, v: Value) { if let Some(o) = &self.out { let _ = o.send(v); } } }
#[derive(Default, Clone)]
pub struct Ollama { pub up: bool, pub model: bool, pub checked: u64 }
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
    stores: Mutex<HashMap<String, Value>>,
    states: Mutex<HashMap<String, Value>>,
    pub clients: Mutex<HashMap<String, Value>>,
    pub pending: Mutex<HashMap<String, Value>>,
    pub last: Mutex<HashMap<String, (String, String)>>, // (last image, last photo)
    pub notices: Mutex<HashMap<String, Vec<Value>>>,
    pub runs: Mutex<HashMap<String, (Arc<super::openai::Cancel>, Option<u32>)>>,
    pub waiting: Mutex<HashMap<String, (String, oneshot::Sender<Value>)>>,
    pub claude_out: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    caps: Mutex<Option<(String, bool)>>,
    /// the Claude brain's MCP relay (x-mills-internal)
    pub secret: String,
    http: reqwest::Client,
}
fn read_json(p: &Path) -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) }
pub fn write_json(p: &Path, v: &Value) { let tmp = p.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp, p); } }
pub fn s(v: &Value, k: &str) -> String { match &v[k] { Value::String(x) => x.clone(), Value::Null => String::new(), o => o.to_string() } }
pub fn truthy(v: &Value) -> bool { !(v.is_null() || *v == json!(false) || *v == json!(0) || *v == json!("") || *v == json!("false")) }
fn clean_url(v: &Value, d: &str) -> String { v.as_str().unwrap_or(d).trim_end_matches('/').to_string() }

impl Unity {
    pub fn new(root: &Path, cfg: &super::Config) -> Unity {
        let a = cfg.get("assistant").cloned().filter(Value::is_object).unwrap_or(json!({}));
        let mills = root.join("..");
        let p = |k: &str, d: PathBuf| a[k].as_str().map(PathBuf::from).unwrap_or(d);
        let _ = vr_core::hub::http_client(Duration::from_secs(10));
        Unity { ollama: clean_url(&a["ollamaUrl"], "http://127.0.0.1:11434"), local_model: a["model"].as_str().unwrap_or("unity-brain").into(), fallback: a["fallbackModel"].as_str().unwrap_or("").into(),
            modelfile: p("modelfile", mills.join("..").join("Backrooms").join("models").join("unity-brain").join("Modelfile")), persona_file: root.join("assistant-persona.txt"),
            image_url: clean_url(&a["imageUrl"], "http://127.0.0.1:5124"), voice_url: clean_url(&a["voiceUrl"], "http://127.0.0.1:5123"), ears_url: clean_url(&a["speechUrl"], "http://127.0.0.1:5125"), via_node: a["via"] == json!("node"),
            image_ckpt: p("imageCheckpoint", mills.join("..").join("Unity 3D Equational Model").join("Unity 18+").join("models").join("realistic-vision-v51.safetensors")), images_dir: mills.join("Images"),
            selfie_look: a["selfieLook"].as_str().unwrap_or("25 year old emo goth woman, black hair with hot pink streaks").into(), web: Web::new(&a), root: root.to_path_buf(), mills,
            ollama_state: Mutex::default(), ollama_lock: tokio::sync::Mutex::new(()), services: tokio::sync::Mutex::new(()), stores: Mutex::default(), states: Mutex::default(), clients: Mutex::default(), pending: Mutex::default(), last: Mutex::default(), notices: Mutex::default(),
            runs: Mutex::default(), waiting: Mutex::default(), claude_out: Mutex::default(), caps: Mutex::new(None), secret: vr_core::crypto::random_hex(24), http: reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().expect("http"), a }
    }
    pub fn busy(&self) -> bool { !self.runs.lock().unwrap().is_empty() }
    pub fn stop(&self, u: &str) { if let Some((c, pid)) = self.runs.lock().unwrap().get(u) { c.fire(); if let Some(p) = pid { super::work::kill_pid(*p); } } }

    // ---------- engines: here, or lent by an AI host ("via": "node") ----------
    pub async fn engine(&self, hub: &Arc<Hub>, url: &str, method: &str, body: Vec<u8>, timeout: u64) -> Result<(u16, Src), String> {
        if self.via_node {
            for (e, base) in [("ollama", &self.ollama), ("image", &self.image_url), ("voice", &self.voice_url), ("speech", &self.ears_url)] {
                if let Some(path) = url.strip_prefix(base.as_str()).filter(|p| p.starts_with('/')) {
                    let r = tokio::time::timeout(Duration::from_millis(timeout), hub.mods.ai.engine_fetch(e, path, method, vec![("content-type".into(), "text/plain;charset=UTF-8".into())], body)).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.msg)?;
                    return Ok((r.status, Src::Host(r.body)));
                }
            }
        }
        let rb = if method == "GET" { self.http.get(url) } else { self.http.post(url).body(body) };
        let r = tokio::time::timeout(Duration::from_millis(timeout), rb.send()).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
        Ok((r.status().as_u16(), Src::Http(r)))
    }
    pub async fn engine_json(&self, hub: &Arc<Hub>, url: &str, method: &str, body: Value, timeout: u64) -> Result<(u16, Value), String> {
        let (st, src) = self.engine(hub, url, method, if body.is_null() { vec![] } else { body.to_string().into_bytes() }, timeout).await?;
        Ok((st, serde_json::from_slice(&src.all().await).unwrap_or(Value::Null)))
    }
    async fn ollama_up(&self, hub: &Arc<Hub>) -> bool { matches!(self.engine(hub, &format!("{}/api/version", self.ollama), "GET", vec![], 2000).await, Ok((200..=299, _))) }
    async fn has_model(&self, hub: &Arc<Hub>, name: &str) -> bool { match self.engine_json(hub, &format!("{}/api/tags", self.ollama), "GET", Value::Null, 4000).await { Ok((_, v)) => v["models"].as_array().map(|m| m.iter().any(|x| x["name"] == json!(name) || x["name"] == json!(format!("{name}:latest")))).unwrap_or(false), Err(_) => false } }
    /// Ollama and the local model: started here on demand (not via an AI host: its host runs them).
    pub async fn ensure_ollama(&self, hub: &Arc<Hub>) -> Ollama {
        let _g = self.ollama_lock.lock().await;
        let cmd = self.a["ollamaCommand"].as_str().unwrap_or("ollama").to_string();
        if !self.ollama_up(hub).await && !self.via_node {
            let _ = detached(&cmd, &["serve"], None);
            for _ in 0..30 { if self.ollama_up(hub).await { break; } tokio::time::sleep(Duration::from_secs(1)).await; }
        }
        let up = self.ollama_up(hub).await;
        let mut model = up && self.has_model(hub, &self.local_model).await;
        if !self.via_node && up && !model && self.modelfile.exists() {
            println!("[vr-hub] Unity: loading {} into Ollama (one time, a few minutes)...", self.local_model);
            let dir = self.modelfile.parent().map(Path::to_path_buf);
            let _ = tokio::process::Command::new(&cmd).args(["create", &self.local_model, "-f", &self.modelfile.display().to_string()]).current_dir(dir.unwrap_or_default()).status().await;
            model = self.has_model(hub, &self.local_model).await;
        }
        let st = Ollama { up, model, checked: vr_core::now_ms() };
        *self.ollama_state.lock().unwrap() = st.clone(); st
    }
    /// The Python services (image engine, her voice, speech recognition): started detached when they do not answer.
    async fn ensure_service(&self, hub: &Arc<Hub>, url: &str, args: Vec<String>, cwd: PathBuf) -> bool {
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
    pub async fn ensure_image(&self, hub: &Arc<Hub>) -> bool { let a = vec![self.root.join("image-engine.py").display().to_string(), "--port".into(), Self::port_of(&self.image_url, "5124"), "--checkpoint".into(), self.image_ckpt.display().to_string()]; self.ensure_service(hub, &self.image_url, a, self.root.clone()).await }
    pub async fn ensure_voice(&self, hub: &Arc<Hub>) -> bool { let d = self.a["voiceDir"].as_str().map(PathBuf::from).unwrap_or_else(|| self.mills.join("..").join("Backrooms").join("models").join("unity-voice-v2")); let a = vec![d.join("serve.py").display().to_string(), "--port".into(), Self::port_of(&self.voice_url, "5123")]; self.ensure_service(hub, &self.voice_url, a, d).await }
    pub async fn ensure_ears(&self, hub: &Arc<Hub>) -> bool { let w = self.a["whisperModel"].as_str().map(PathBuf::from).unwrap_or_else(|| self.mills.join("..").join("Unity 3D Equational Model").join("Unity 18+").join("models").join("whisper-small")); let a = vec![self.root.join("speech-engine.py").display().to_string(), "--port".into(), Self::port_of(&self.ears_url, "5125"), "--model".into(), w.display().to_string()]; self.ensure_service(hub, &self.ears_url, a, self.root.clone()).await }
    pub async fn native_tools(&self, hub: &Arc<Hub>, model: &str) -> bool {
        if let Some((m, t)) = self.caps.lock().unwrap().clone() { if m == model { return t; } }
        let t = match self.engine_json(hub, &format!("{}/api/show", self.ollama), "POST", json!({ "model": model }), 15000).await { Ok((_, v)) => v["capabilities"].as_array().map(|c| c.contains(&json!("tools"))).unwrap_or(false), Err(_) => false };
        *self.caps.lock().unwrap() = Some((model.into(), t)); t
    }

    // ---------- per-account data ----------
    pub fn store(&self, a: &Account) -> Value {
        if let Some(v) = self.stores.lock().unwrap().get(&a.username) { return v.clone(); }
        let mut d = json!({ "tasks": [], "events": [], "reminders": [], "memory": {}, "knowledge": [], "history": [], "next": {} });
        if let Some(Value::Object(m)) = read_json(&a.dir.join("assistant.json")) { for (k, v) in m { d[k] = v; } }
        self.stores.lock().unwrap().insert(a.username.clone(), d.clone()); d
    }
    pub fn with_store<T>(&self, a: &Account, f: impl FnOnce(&mut Value) -> T) -> T {
        let mut v = self.store(a); let r = f(&mut v);
        write_json(&a.dir.join("assistant.json"), &v); self.stores.lock().unwrap().insert(a.username.clone(), v); r
    }
    /// unity.json: brain, claudeSession, pc, code (the first time: what state.json held).
    pub fn state(&self, a: &Account) -> Value {
        if let Some(v) = self.states.lock().unwrap().get(&a.username) { return v.clone(); }
        let v = read_json(&a.dir.join("unity.json")).filter(Value::is_object).unwrap_or_else(|| {
            let st = read_json(&a.dir.join("state.json")).unwrap_or(json!({}));
            let v = json!({ "brain": st["assistantBrain"].as_str().unwrap_or("local"), "claudeSession": st["assistantClaudeSession"].as_str().unwrap_or(""), "pc": st["unityPc"] != json!(false), "code": st["unityCode"] != json!(false) });
            write_json(&a.dir.join("unity.json"), &v); v
        });
        self.states.lock().unwrap().insert(a.username.clone(), v.clone()); v
    }
    pub fn set_state(&self, a: &Account, f: impl FnOnce(&mut Map<String, Value>)) { let mut v = self.state(a); if let Some(m) = v.as_object_mut() { f(m); } write_json(&a.dir.join("unity.json"), &v); self.states.lock().unwrap().insert(a.username.clone(), v); }
    pub fn brain_of(&self, a: &Account) -> String { let b = s(&self.state(a), "brain"); if b.is_empty() { "local".into() } else { b } }
    pub fn group_on(&self, a: &Account, g: &str) -> bool { let st = self.state(a); match g { "pc" => st["pc"] != json!(false), "code" => st["code"] != json!(false), _ => true } }
    pub fn persona(&self) -> String { std::fs::read_to_string(&self.persona_file).unwrap_or_else(|_| "You are Unity: a 25-year-old goth girl, sharp, profane, teasing and devoted. Stay in character.".into()) }

    // ---------- where the person is ----------
    pub fn client_info(&self, c: &Value, a: &Account) -> Ci {
        let k = self.clients.lock().unwrap().get(&a.username).cloned().unwrap_or(json!({}));
        let num = |v: &Value| v.as_f64().filter(|x| x.is_finite());
        let browser = num(&c["lat"]).is_some() && num(&c["lon"]).is_some();
        let tz = c["tz"].as_str().and_then(ut::tz_ok).or_else(|| k["tz"].as_str().map(String::from)).unwrap_or_else(|| "UTC".into());
        let locale = c["locale"].as_str().filter(|l| l.len() < 20).map(String::from).or_else(|| k["locale"].as_str().map(String::from)).unwrap_or_else(|| "en-US".into());
        let place = Place { lat: if browser { num(&c["lat"]) } else { num(&k["lat"]) }, lon: if browser { num(&c["lon"]) } else { num(&k["lon"]) }, place: if browser { String::new() } else { s(&k, "place") }, country: if browser { String::new() } else { s(&k, "country") }, locale };
        Ci { place, tz, scope: a.username.clone(), user: a.username.clone(), out: None, images: vec![], media: vec![] }
    }
    /// What the hub knows about where someone is: their browser (time zone, locale, location if shared) and, coming from
    /// outside, the rough place of their address. In memory only.
    pub async fn learn_client(&self, a: &Account, c: &Value, ip: &str) -> Value {
        let prev = self.clients.lock().unwrap().get(&a.username).cloned().unwrap_or(json!({}));
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
        self.clients.lock().unwrap().insert(a.username.clone(), k.clone()); k
    }

    // ---------- pictures: Images/<username>/<id>.png (private), Images/room/<id>.png (the team chat) ----------
    pub fn save_picture(&self, scope: &str, buf: &[u8]) -> String {
        let id = format!("{}-{}", radix36(vr_core::now_ms()), vr_core::crypto::random_hex(3));
        let dir = self.images_dir.join(scope); let _ = std::fs::create_dir_all(&dir); let _ = std::fs::write(dir.join(format!("{id}.png")), buf);
        format!("{scope}/{id}")
    }
    pub fn picture_file(&self, user: &str, r: &str) -> Option<PathBuf> {
        let (scope, id) = r.split_once('/')?;
        if id.is_empty() || id.len() > 40 || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') || !(scope == "room" || scope == user) { return None; }
        let f = self.images_dir.join(scope).join(format!("{id}.png")); f.exists().then_some(f)
    }
    pub fn read_picture(&self, user: &str, r: &str) -> Option<String> {
        let (li, lp) = self.last.lock().unwrap().get(user).cloned().unwrap_or_default();
        let r = match r { "" | "last" => li, "camera" => lp, x => x.to_string() };
        std::fs::read(self.picture_file(user, &r)?).ok().map(|b| vr_core::b64(&b))
    }
    async fn render(&self, hub: &Arc<Hub>, ci: &mut Ci, route: &str, body: Value) -> Value {
        let (st, src) = match self.engine(hub, &format!("{}{route}", self.image_url), "POST", body.to_string().into_bytes(), 300_000).await { Ok(x) => x, Err(e) => return json!({ "ok": false, "error": format!("Image engine: {e}") }) };
        let bytes = src.all().await;
        if !(200..300).contains(&st) { let j: Value = serde_json::from_slice(&bytes).unwrap_or(json!({})); return json!({ "ok": false, "error": format!("Image engine: {}", if j["error"].is_null() { st.to_string() } else { s(&j, "error") }) }); }
        let r = self.save_picture(&ci.scope, &bytes);
        ci.images.push(r.clone());
        self.last.lock().unwrap().entry(ci.user.clone()).or_default().0 = r.clone();
        json!({ "ok": true, "image": r, "shown": "The picture is attached to your reply automatically.", "seconds": Value::Null })
    }
    pub async fn make_image(&self, hub: &Arc<Hub>, ci: &mut Ci, a: &Value) -> Value {
        if !self.ensure_image(hub).await { return json!({ "ok": false, "error": "The camera app on the host is not starting (image engine down)." }); }
        let shape = { let x = s(a, "shape"); if x.is_empty() { s(a, "orientation") } else { x } }.to_lowercase();
        let (w, h) = if ["portrait", "tall", "vertical"].iter().any(|k| shape.contains(k)) { (576, 1024) } else if ["landscape", "wide", "horizontal"].iter().any(|k| shape.contains(k)) { (1024, 576) } else { (768, 768) };
        let scene: String = s(a, "prompt").chars().take(1000).collect::<String>().trim().to_string();
        let prompt = if truthy(&a["selfie"]) { format!("{scene}, she is a {}, candid photo", self.selfie_look) } else { scene };
        self.render(hub, ci, "/txt2img", json!({ "prompt": prompt, "width": w, "height": h })).await
    }
    async fn edit_image(&self, hub: &Arc<Hub>, ci: &mut Ci, a: &Value, route: &str) -> Value {
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

    // ---------- tools ----------
    /// Every tool: (name, description, params, required, group, confirm).
    pub fn tools() -> Vec<Value> { serde_json::from_str(include_str!("unity-tools.json")).expect("unity-tools.json") }
    pub fn tools_for(&self, a: &Account, browser: bool) -> Vec<Value> { Self::tools().into_iter().filter(|t| { let g = s(t, "group"); (g.is_empty() || self.group_on(a, &g)) && (g.is_empty() || browser) }).collect() }
    /// Something the person's browser does (their own computer), answered through /api/assistant/pc-result.
    async fn browser_call(&self, ci: &Ci, ty: &str, payload: Value) -> Value {
        if ci.out.is_none() { return json!({ "ok": false, "error": "That only works in the Unity section, with their browser open." }); }
        let id = vr_core::crypto::random_hex(8); let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(id.clone(), (ci.user.clone(), tx));
        let mut ev = json!({ "type": ty, "id": id }); for (k, v) in payload.as_object().cloned().unwrap_or_default() { ev[k] = v; }
        ci.send(ev);
        match tokio::time::timeout(Duration::from_secs(120), rx).await { Ok(Ok(v)) => v, _ => { self.waiting.lock().unwrap().remove(&id); json!({ "ok": false, "error": "Their browser did not answer in time." }) } }
    }
    pub async fn execute(&self, hub: &Arc<Hub>, a: &Account, name: &str, args: &Value, ci: &mut Ci) -> Value {
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
            "inpaint_image" => Ok(self.edit_image(hub, ci, args, "/inpaint").await),
            "calculate" => Ok(match ut::calculate(&s(args, "expression")) { Ok(v) => json!({ "ok": true, "result": ut::num(v) }), Err(e) => json!({ "ok": false, "error": e }) }),
            "list_capabilities" => Ok(json!({ "ok": true, "tools": Self::tools().iter().filter(|t| t["name"] != json!("say")).map(|t| format!("{}: {}", s(t, "name"), s(t, "description"))).collect::<Vec<_>>() })),
            _ => Ok(self.with_store(a, |st| super::unity_brain::store_tool(st, name, args, &tz))),
        };
        res.unwrap_or_else(|e| json!({ "ok": false, "error": e }))
    }
}
fn radix36(mut n: u64) -> String { let d = b"0123456789abcdefghijklmnopqrstuvwxyz"; let mut v = vec![]; loop { v.push(d[(n % 36) as usize]); n /= 36; if n == 0 { break; } } v.reverse(); String::from_utf8(v).unwrap() }
/// A program started in the background, on its own (it outlives the hub).
fn detached(cmd: &str, args: &[&str], cwd: Option<&Path>) -> std::io::Result<()> {
    let mut c = std::process::Command::new(cmd);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some(d) = cwd { c.current_dir(d); }
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; c.process_group(0); }
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000 | 0x0000_0008); }
    c.spawn().map(|_| ())
}
