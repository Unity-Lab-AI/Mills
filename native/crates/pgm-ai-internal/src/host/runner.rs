//! The AI host itself: hello, the poll loop (the heartbeat both ways), jobs, the status file, the control requests,
//! updates. The same behaviour as the JavaScript host (bridge/ai-node.js), so the hub cannot tell them apart except by
//! the version and the extra capability ("update-bin") they announce.
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::host::folder::{log, Folder};
use crate::host::{relay, work};
use crate::host::settings::{HostSettings, Provider};
use pgm_core_internal::{now_ms, Error, Result};
use pgm_net_internal::hostlink::{Hello, HubLink, Job, OfferedProvider, Poll};

pub const VERSION: &str = match option_env!("VR_VERSION") { Some(v) => v, None => env!("CARGO_PKG_VERSION") };
pub const COMMIT: &str = match option_env!("VR_COMMIT") { Some(v) => v, None => "" };
pub const TARGET: &str = env!("VR_TARGET");

pub struct Host {
    pub folder: Folder,
    pub cfg: HostSettings,
    pub link: RwLock<HubLink>,
    pub http: reqwest::Client,
    pub nickname: Mutex<String>,
    pub status: Mutex<Map<String, Value>>,
    status_dirty: Mutex<bool>,
    pub active: AtomicUsize,
    pub last_job_at: AtomicU64,
    pub runs: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    /// provider id → { five_hour, seven_day } (this host's own view of its Claude windows)
    pub usage: Mutex<HashMap<String, Value>>,
    updating: Mutex<bool>,
    /// What the host found running on this machine by itself (a local Ollama): offered next to the configured ones.
    pub detected: Mutex<Detected>,
}
pub type H = Arc<Host>;

/// What the host found on this machine without being told: an Ollama that answers on its usual address.
#[derive(Default, Clone)]
pub struct Detected { pub ollama_engine: Option<String>, pub ollama_provider: Option<Provider> }

pub const OLLAMA_URL: &str = "http://127.0.0.1:11434";

/// Where to reach Ollama from its OLLAMA_HOST setting, which is the address it LISTENS on: "0.0.0.0" / "[::]" (every
/// interface) is reached at 127.0.0.1, a missing port is Ollama's 11434, a missing scheme is http.
pub fn ollama_url_of(v: &str) -> String {
    let v = v.trim().trim_end_matches('/');
    if v.is_empty() { return OLLAMA_URL.to_string(); }
    let (scheme, rest) = match v.split_once("://") { Some((s, r)) => (s.to_string(), r.to_string()), None => ("http".to_string(), v.to_string()) };
    let (host, port) = match rest.rsplit_once(':') { Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && !h.ends_with('[') => (h.to_string(), p.to_string()), _ => (rest.clone(), "11434".to_string()) };
    let host = match host.as_str() { "" | "0.0.0.0" | "[::]" | "::" => "127.0.0.1".to_string(), h => h.to_string() };
    format!("{scheme}://{host}:{port}")
}

/// The model names of an Ollama /api/tags answer, in the order it gave them (empty when it is not that).
pub fn ollama_models(tags: &Value) -> Vec<String> {
    tags["models"].as_array().map(|a| a.iter().filter_map(|m| m["name"].as_str().or(m["model"].as_str()).map(String::from)).take(40).collect()).unwrap_or_default()
}

/// Is there an Ollama here that answers, and which models does it have? Offered without any configuration: the engine
/// "ollama" (Unity) when none is configured, and a private provider "ollama" (an OpenAI-compatible endpoint at /v1) the
/// person can pick for the API and Unity. Off with "autoDetect": { "ollama": false } in ai-node.json; a provider or engine
/// the person configured themselves is never replaced. Runs at every hello, so an Ollama started later is found on the
/// next reconnect and one that went away is dropped.
pub async fn detect_local(h: &H) {
    let off = h.cfg.raw.get("autoDetect").and_then(|o| o.get("ollama")).and_then(Value::as_bool) == Some(false);
    let url = std::env::var("OLLAMA_HOST").ok().map(|u| ollama_url_of(&u)).unwrap_or_else(|| OLLAMA_URL.to_string());
    let mut d = Detected::default();
    if !off {
        let tags: Option<Value> = match h.http.get(format!("{url}/api/tags")).timeout(Duration::from_millis(1500)).send().await { Ok(r) if r.status().is_success() => r.json().await.ok(), _ => None };
        if let Some(tags) = tags {
            let models = ollama_models(&tags);
            let configured_engine = h.cfg.engines().iter().any(|(k, _)| k == "ollama");
            let configured_endpoint = h.cfg.all_providers().iter().any(|p| p.kind == "openai" && (p.base_url.contains(":11434") || p.id == "ollama"));
            if !configured_engine { d.ollama_engine = Some(url.clone()); }
            if !configured_endpoint && !models.is_empty() {
                d.ollama_provider = Some(Provider { id: "ollama".into(), kind: "openai".into(), label: format!("{}'s Ollama (this machine)", if h.cfg.user().is_empty() { "this host".to_string() } else { h.cfg.user() }), models, enabled: true,
                    share_with_group: false, allowed_users: vec![], max5h: None, max7d: None, base_url: format!("{url}/v1"), api_key: String::new(), config_dir: String::new(), codex_home: String::new(), command: String::new() });
            }
        }
    }
    let was = h.detected.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (a, b) = (was.ollama_engine.is_some(), d.ollama_engine.is_some());
    if a != b || was.ollama_provider.as_ref().map(|p| &p.models) != d.ollama_provider.as_ref().map(|p| &p.models) {
        match &d.ollama_provider { Some(p) => log(format!("found Ollama at {url} with {} model(s): offered as the engine \"ollama\" and as a private provider \"ollama\" ({})", p.models.len(), p.models.iter().take(5).cloned().collect::<Vec<_>>().join(", "))),
            None if b => log(format!("found Ollama at {url} (no models pulled yet): offered as the engine \"ollama\"")),
            None => log("no Ollama answers on this machine (start it, or set engines.ollama in ai-node.json)") }
    }
    *h.detected.lock().unwrap_or_else(|e| e.into_inner()) = d;
}

/// The logins this host lends, checked where the relay will look for them, so a missing one is said here and not only as
/// a 503 on someone's request. Codex keeps its login in the system keyring by default on some systems; the host can only
/// read the auth.json file, so that case gets its own explanation.
pub fn check_logins(h: &H) {
    for p in h.providers() {
        if !p.is_relay() { continue; }
        if let Some(msg) = relay::login_problem(&p) { log(format!("provider \"{}\": {msg}", p.id)); }
    }
}

impl Host {
    pub fn set_status(&self, patch: Value) {
        if let Value::Object(p) = patch { let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner()); for (k, v) in p { s.insert(k, v); } s.insert("at".into(), json!(now_ms())); }
        *self.status_dirty.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }
    pub fn write_status_now(&self) {
        *self.status_dirty.lock().unwrap_or_else(|e| e.into_inner()) = false;
        let text = serde_json::to_string_pretty(&Value::Object(self.status.lock().unwrap_or_else(|e| e.into_inner()).clone())).unwrap_or_default();
        let tmp = self.folder.status().with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() { let _ = std::fs::rename(&tmp, self.folder.status()); }
    }
    pub fn quiet(&self, ms: u64) -> bool { self.runs.lock().unwrap_or_else(|e| e.into_inner()).is_empty() && self.active.load(Ordering::SeqCst) == 0 && now_ms().saturating_sub(self.last_job_at.load(Ordering::SeqCst)) >= ms }
    pub fn busy(&self) -> bool { !self.runs.lock().unwrap_or_else(|e| e.into_inner()).is_empty() || self.active.load(Ordering::SeqCst) > 0 }
    /// The configured providers plus a local Ollama the host found by itself (see detect_local).
    pub fn providers(&self) -> Vec<Provider> {
        let mut v = self.cfg.providers();
        let d = self.detected.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = &d.ollama_provider { if !v.iter().any(|x| x.id == p.id) { v.push(p.clone()); } }
        v
    }
    /// The engines lent to the hub by name: the configured ones, plus "ollama" when one answers here and none is set.
    pub fn engines(&self) -> Vec<(String, String)> {
        let mut v = self.cfg.engines();
        let d = self.detected.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(u) = &d.ollama_engine { if !v.iter().any(|(k, _)| k == "ollama") { v.push(("ollama".into(), u.clone())); } }
        v
    }
    /// Is this provider at the usage cap its owner set? The reason, or "".
    pub fn over_cap(&self, p: &Provider) -> String {
        let u = self.usage.lock().unwrap_or_else(|e| e.into_inner()).get(&p.id).cloned().unwrap_or(Value::Null);
        let pc = |w: &Value| -> f64 { let ut = w.get("utilization").and_then(Value::as_f64); let rs = w.get("resetsAt").and_then(Value::as_f64); match ut { Some(x) if rs.map(|r| r > now_ms() as f64).unwrap_or(true) => x * 100.0, _ => 0.0 } };
        if let Some(m) = p.max5h { if pc(&u["five_hour"]) >= m { return format!("this host's 5-hour Claude usage is at its {m}% cap"); } }
        if let Some(m) = p.max7d { if pc(&u["seven_day"]) >= m { return format!("this host's weekly Claude usage is at its {m}% cap"); } }
        String::new()
    }
    /// Counts a job in flight for as long as `f` runs (an update or restart waits for it).
    pub async fn track<F: std::future::Future<Output = ()>>(&self, f: F) {
        self.active.fetch_add(1, Ordering::SeqCst); self.last_job_at.store(now_ms(), Ordering::SeqCst);
        f.await;
        self.active.fetch_sub(1, Ordering::SeqCst); self.last_job_at.store(now_ms(), Ordering::SeqCst);
    }
}

/// One host per folder: the lock file holds the running pid. Some(code) = do not run (2 = already running).
fn single_instance(folder: &Folder, update_flag: bool) -> Option<i32> {
    if let Some(pid) = folder.host_pid() {
        if pid != std::process::id() {
            if update_flag { let _ = std::fs::write(folder.update_now(), now_ms().to_string()); println!("[ai-host] asked the running host (pid {pid}) to update from the hub now; it does so as soon as nothing is in flight."); return Some(0); }
            println!("[ai-host] already running (pid {pid}); nothing to do."); return Some(2);
        }
    }
    let _ = std::fs::write(folder.lock(), std::process::id().to_string());
    None
}
fn release_lock(folder: &Folder) {
    if std::fs::read_to_string(folder.lock()).ok().and_then(|t| t.trim().parse::<u32>().ok()) == Some(std::process::id()) { let _ = std::fs::remove_file(folder.lock()); }
}

/// The trial after a binary update: a version that has not reached the hub in 3 starts is put back.
fn trial_start(folder: &Folder) -> Option<i32> {
    let mut t = folder.read_json(&folder.trial())?;
    let starts = t["starts"].as_u64().unwrap_or(0) + 1;
    t["starts"] = json!(starts);
    if starts <= 3 { let _ = std::fs::write(folder.trial(), t.to_string()); return None; }
    println!("[ai-host] the update to {} has not reached the hub in 3 starts: going back to {}", t["to"].as_str().unwrap_or("?"), t["from"].as_str().unwrap_or("?"));
    if let Ok(exe) = std::env::current_exe() { if let Err(e) = pgm_dist_internal::update::roll_back(&exe) { println!("[ai-host] could not go back: {e}"); } }
    let _ = std::fs::write(folder.bad(), t["to"].as_str().unwrap_or(""));
    let _ = std::fs::remove_file(folder.trial());
    Some(3) // the supervisor (vr-host run) starts the old version again
}
fn update_confirmed(h: &Host) {
    let Some(t) = h.folder.read_json(&h.folder.trial()) else { return };
    let _ = std::fs::remove_file(h.folder.trial());
    if let Ok((_, _, old)) = pgm_dist_internal::update::self_paths() { let _ = std::fs::remove_file(old); }
    note_update(h, &format!("update to {} confirmed: it reached the hub", t["to"].as_str().unwrap_or("the new version")));
}
pub fn note_update(h: &Host, text: &str) { log(text); h.set_status(json!({ "lastUpdate": text, "lastUpdateAt": now_ms() })); }

/// Runs the host until it should exit; the exit code (0 stop, 2 already running, 3 restart: new code / config / update).
pub async fn run(folder: Folder, selftest: bool, update_flag: bool) -> i32 {
    let cfg = match HostSettings::load(&folder.config) { Ok(c) => c, Err(e) => { eprintln!("[ai-host] {e}"); return 1; } };
    if let Err(e) = cfg.check() { eprintln!("[ai-host] {e}"); return 1; }
    if pgm_core_internal::fs::readable_by_others(&folder.config) { pgm_core_internal::fs::private(&folder.config); println!("[ai-host] made ai-node.json readable by its owner only (it holds the token)"); }
    if selftest { println!("[ai-host] selftest: {VERSION} loads ({TARGET})"); return 0; }
    if let Some(code) = single_instance(&folder, update_flag) { return code; }
    if let Some(code) = trial_start(&folder) { release_lock(&folder); return code; }
    let link = match HubLink::new(&cfg.hub(), &cfg.id(), &cfg.token(), VERSION) { Ok(l) => l, Err(e) => { eprintln!("[ai-host] {e}"); release_lock(&folder); return 1; } };
    let http = match pgm_net_internal::client::http_client(Duration::from_secs(20)) { Ok(c) => c, Err(e) => { eprintln!("[ai-host] {e}"); release_lock(&folder); return 1; } };
    let status = json!({ "pid": std::process::id(), "version": VERSION, "native": true, "target": TARGET, "hub": cfg.hub(), "id": cfg.id(), "startedAt": now_ms(), "connected": false, "connectedAt": 0, "lastOkAt": 0, "downSince": 0, "lastError": "", "active": 0, "runs": 0, "lastJobAt": 0, "paused": false, "lastUpdate": "", "lastUpdateAt": 0 });
    let h: H = Arc::new(Host { nickname: Mutex::new(cfg.nickname()), folder: folder.clone(), cfg, link: RwLock::new(link), http, status: Mutex::new(status.as_object().cloned().unwrap_or_default()), status_dirty: Mutex::new(true),
        active: AtomicUsize::new(0), last_job_at: AtomicU64::new(0), runs: Mutex::new(HashMap::new()), usage: Mutex::new(HashMap::new()), updating: Mutex::new(false), detected: Mutex::new(Detected::default()) });
    h.write_status_now();
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::channel::<i32>(4);
    background(h.clone(), exit_tx.clone());
    let main = tokio::spawn(main_loop(h.clone(), exit_tx.clone()));
    let code = tokio::select! {
        c = exit_rx.recv() => c.unwrap_or(0),
        _ = shutdown_signal() => 0,
    };
    main.abort();
    for (_, stop) in h.runs.lock().unwrap_or_else(|e| e.into_inner()).drain() { let _ = stop.send(()); }
    h.set_status(json!({ "connected": false })); h.write_status_now();
    release_lock(&folder);
    code
}
async fn shutdown_signal() {
    #[cfg(unix)]
    { use tokio::signal::unix::{signal, SignalKind}; let mut t = signal(SignalKind::terminate()).expect("signal"); let mut hup = signal(SignalKind::hangup()).expect("signal"); tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = t.recv() => {}, _ = hup.recv() => {} } }
    #[cfg(not(unix))]
    { let _ = tokio::signal::ctrl_c().await; }
}

/// The status writer, the control requests, the code / config watcher and the update timer.
fn background(h: H, exit: tokio::sync::mpsc::Sender<i32>) {
    let h1 = h.clone();
    tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_millis(500)).await; if *h1.status_dirty.lock().unwrap_or_else(|e| e.into_inner()) { h1.write_status_now(); } } });
    let h2 = h.clone(); let e2 = exit.clone();
    tokio::spawn(async move {
        let mut tick = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await; tick += 1;
            if tick % 3 == 0 { h2.set_status(json!({ "active": h2.active.load(Ordering::SeqCst), "runs": h2.runs.lock().unwrap_or_else(|e| e.into_inner()).len(), "lastJobAt": h2.last_job_at.load(Ordering::SeqCst), "paused": h2.folder.off().exists() })); }
            for (file, code, what) in [(h2.folder.stop_now(), 0, "stopping"), (h2.folder.restart_now(), 3, "restarting")] {
                if !file.exists() { continue; }
                let how = std::fs::read_to_string(&file).unwrap_or_default();
                if how.trim() != "now" && h2.busy() { h2.set_status(json!({ "pending": format!("{what} once nothing is in flight") })); continue; }
                let _ = std::fs::remove_file(&file);
                log(format!("{what}, as asked by the control app")); h2.set_status(json!({ "connected": false, "pending": what })); h2.write_status_now();
                let _ = e2.send(code).await; return;
            }
        }
    });
    // new code (the binary was replaced by hand) or new settings: restart on them at a quiet moment
    let h3 = h.clone(); let e3 = exit.clone();
    tokio::spawn(async move {
        let stamp = |h: &Host| -> String { [h.folder.config.clone(), std::env::current_exe().unwrap_or_default()].iter().map(|f| std::fs::metadata(f).and_then(|m| m.modified()).map(|t| format!("{t:?}")).unwrap_or_default()).collect::<Vec<_>>().join(",") };
        let started = stamp(&h3);
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let now = stamp(&h3);
            if now != started && h3.quiet(60000) && !own_config_write(&h3) { log("new code or config here: restarting on it"); let _ = e3.send(3).await; return; }
            if h3.folder.update_now().exists() && !h3.busy() { let _ = std::fs::remove_file(h3.folder.update_now()); log("asked to update now"); check_update(h3.clone(), true, e3.clone()).await; }
        }
    });
    let h4 = h.clone();
    tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_millis(h4.cfg.update_check_ms())).await; check_update(h4.clone(), false, exit.clone()).await; } });
    // an Ollama started (or stopped, or given new models) after the host came up: looked for every minute, and a change
    // is told to the hub with a fresh hello (the session key is renewed with it; nothing in flight is cut off)
    let h5 = h.clone();
    tokio::spawn(async move { loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let before = offered(&h5);
        detect_local(&h5).await;
        if offered(&h5) != before && h5.link.read().await.key.is_some() && !h5.busy() { log("what this host offers changed: saying hello again"); h5.link.write().await.key = None; }
    } });
}
/// What a hello would offer (provider ids + models, engine names): a change means a new hello.
fn offered(h: &Host) -> String { format!("{:?}|{:?}", h.providers().iter().map(|p| (p.id.clone(), p.models.clone())).collect::<Vec<_>>(), h.engines().iter().map(|e| e.0.clone()).collect::<Vec<_>>()) }
/// The host writes its own nickname into the config after hello: not a change to restart on.
fn own_config_write(h: &Host) -> bool {
    let Ok(now) = HostSettings::load(&h.folder.config) else { return false };
    let mut a = now.raw.clone(); let mut b = h.cfg.raw.clone();
    a.remove("nickname"); b.remove("nickname");
    a == b
}

fn hello_body(h: &Host) -> Hello {
    let providers = h.providers().iter().map(|p| {
        let mut allowed = p.allowed_users.clone(); if p.share_with_group { allowed.push("@group".into()); }
        OfferedProvider { id: p.id.clone(), kind: p.relay_kind().to_string(), label: p.label.clone(), models: p.models.clone(), allowed_users: allowed, max5h: p.max5h, max7d: p.max7d }
    }).collect();
    Hello { nonce: pgm_core_internal::crypto::random_hex(16), user: h.cfg.user(), providers, engines: h.engines().into_iter().map(|(k, _)| k).collect(), pc: h.cfg.pc_folder().is_some(),
        version: VERSION.to_string(), os: if cfg!(windows) { "win32".into() } else if cfg!(target_os = "macos") { "darwin".into() } else { "linux".into() }, caps: pgm_net_internal::hostlink::CAPS.iter().map(|s| s.to_string()).collect() }
}
async fn hello(h: &H, exit: &tokio::sync::mpsc::Sender<i32>) -> Result<()> {
    detect_local(h).await;
    check_logins(h);
    let body = hello_body(h);
    let a = h.link.write().await.hello(&body).await?;
    let kid = h.link.read().await.key.as_ref().map(|k| k.id()).unwrap_or_default();
    let nick = if a.nickname.is_empty() { h.cfg.nickname() } else { a.nickname.clone() };
    *h.nickname.lock().unwrap_or_else(|e| e.into_inner()) = nick.clone();
    if h.cfg.raw.get("nickname").and_then(Value::as_str) != Some(nick.as_str()) {
        if let Ok(mut c) = HostSettings::load(&h.folder.config) { c.raw.insert("nickname".into(), json!(nick)); let _ = c.save(); }
    }
    let engines: Vec<String> = h.engines().into_iter().map(|(k, _)| k).collect();
    log(format!("connected to {} as \"{nick}\"{}: {} provider(s) [{}], engines {}{}, key {kid}", h.cfg.hub(), if h.cfg.user().is_empty() { String::new() } else { format!(" ({})", h.cfg.user()) },
        body.providers.len(), body.providers.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", "), if engines.is_empty() { "none".into() } else { engines.join(", ") },
        h.cfg.pc_folder().map(|p| format!(", computer access to {}", p.display())).unwrap_or_default()));
    let hv = a.update.as_ref().map(|u| u.version.clone()).unwrap_or_default();
    h.set_status(json!({ "connected": true, "connectedAt": now_ms(), "lastOkAt": now_ms(), "nickname": nick, "kid": kid, "owner": h.cfg.user(), "group": a.group, "hubVersion": hv, "lastError": "" }));
    update_confirmed(h);
    if a.update.map(|u| !u.commit.is_empty()).unwrap_or(false) { let h2 = h.clone(); let e = exit.clone(); tokio::spawn(async move { tokio::time::sleep(Duration::from_secs(5)).await; check_update(h2, false, e).await; }); }
    Ok(())
}

async fn main_loop(h: H, exit: tokio::sync::mpsc::Sender<i32>) {
    let (mut paused, mut misses, mut down_since, mut hub_commit) = (false, 0u64, 0u64, String::new());
    loop {
        if h.folder.off().exists() {
            if !paused { paused = true; log("paused by this machine's host (ai-host.off)"); h.set_status(json!({ "paused": true, "connected": false })); }
            tokio::time::sleep(Duration::from_secs(3)).await; continue;
        }
        if paused { paused = false; log("back on"); h.set_status(json!({ "paused": false })); }
        let step: Result<()> = async {
            if h.link.read().await.key.is_none() { hello(&h, &exit).await?; }
            let poll = h.link.read().await.poll().await?;
            match poll {
                Poll::Rehello => { log("the hub asks for a hello again (it restarted without our session)"); h.link.write().await.key = None; }
                Poll::Unknown => { log("the hub does not know this host (id / token): check ai-node.json"); h.set_status(json!({ "connected": false, "lastError": "the hub does not know this host: check its id and token" })); h.link.write().await.key = None; tokio::time::sleep(Duration::from_secs(30)).await; }
                Poll::Jobs(p) => {
                    if down_since != 0 { log(format!("hub back after {} s", (now_ms() - down_since) / 1000)); down_since = 0; h.set_status(json!({ "connected": true, "connectedAt": now_ms() })); }
                    misses = 0;
                    let mut patch = json!({ "connected": true, "lastOkAt": now_ms(), "downSince": 0, "lastError": "" });
                    if let Some(b) = &p.beat { if !b.version.is_empty() { patch["hubVersion"] = json!(b.version); } patch["hubCommit"] = json!(b.commit); }
                    h.set_status(patch);
                    if let Some(b) = &p.beat { if !b.commit.is_empty() && b.commit != hub_commit {
                        if !hub_commit.is_empty() { log(format!("the hub is now on {}", if b.version.is_empty() { &b.commit[..7.min(b.commit.len())] } else { &b.version })); let (h2, e) = (h.clone(), exit.clone()); tokio::spawn(async move { tokio::time::sleep(Duration::from_secs(15)).await; check_update(h2, false, e).await; }); }
                        hub_commit = b.commit.clone();
                    } }
                    for id in &p.cancel { if let Some(stop) = h.runs.lock().unwrap_or_else(|e| e.into_inner()).remove(id) { log(format!("stopping run {id} (stopped by its person)")); let _ = stop.send(()); } }
                    for job in p.jobs { dispatch(h.clone(), job); }
                }
            }
            Ok(())
        }.await;
        if let Err(e) = step {
            if e.code() == 401 { log(e.to_string()); h.set_status(json!({ "connected": false, "lastError": e.to_string() })); tokio::time::sleep(Duration::from_secs(60)).await; continue; }
            if down_since == 0 { down_since = now_ms(); }
            h.set_status(json!({ "connected": false, "downSince": down_since, "lastError": e.to_string().chars().take(200).collect::<String>() }));
            if misses % 30 == 0 { log(format!("hub not reachable ({e}); reconnecting")); }
            misses += 1;
            tokio::time::sleep(Duration::from_millis(h.cfg.reconnect_max_ms().min(1000 * misses))).await; // 1 s, 2 s … the setting
        }
    }
}

fn dispatch(h: H, job: Job) {
    tokio::spawn(async move {
        if job.kind == "work" {
            if h.cfg.allow_work() { work::work_job(h, job).await; }
            else { h.link.read().await.reply(&job.id, 403, &ct_json(), br#"{"error":"This host shares its AI only, not its computer."}"#).await; }
        } else if job.engine == "pc" { let h2 = h.clone(); h.track(pc_job(h2, job)).await; }
        else { let h2 = h.clone(); h.track(relay_job(h2, job)).await; }
    });
}
pub fn ct_json() -> Map<String, Value> { let mut m = Map::new(); m.insert("content-type".into(), json!("application/json")); m }
fn err_body(msg: &str) -> Vec<u8> { json!({ "error": { "type": "relay_error", "message": msg } }).to_string().into_bytes() }

/// Engine, OpenAI-compatible endpoint and login relay calls: one HTTP call here, the answer back whole or streamed.
async fn relay_job(h: H, job: Job) {
    let out: Result<()> = async {
        if let Some(pid) = job.engine.strip_prefix("relay:") {
            let p = h.providers().into_iter().find(|x| x.id == pid && x.is_relay()).ok_or_else(|| Error::status(404, "This host shares no such login."))?;
            let cap = h.over_cap(&p); if !cap.is_empty() { return Err(Error::status(429, format!("{}: {cap}.", h.nickname.lock().unwrap_or_else(|e| e.into_inner())))); }
            let r = relay::forward(&h, &p, &job).await?;
            if let Some(u) = relay::usage_from(r.headers()) { let mut all = h.usage.lock().unwrap_or_else(|e| e.into_inner()); let e = all.entry(p.id.clone()).or_insert_with(|| json!({})); if let (Some(o), Value::Object(n)) = (e.as_object_mut(), u) { o.extend(n); } }
            let hdrs = relay::pass_headers(r.headers());
            return answer(&h, &job, r, hdrs, true).await;
        }
        let (base, auth) = if let Some(pid) = job.engine.strip_prefix("provider:") {
            let p = h.providers().into_iter().find(|x| x.id == pid && x.kind == "openai").ok_or_else(|| Error::status(404, "This host offers no such endpoint."))?;
            (p.base_url.clone(), if p.api_key.is_empty() { None } else { Some(format!("Bearer {}", p.api_key)) })
        } else {
            let base = h.engines().into_iter().find(|(k, _)| *k == job.engine).map(|(_, u)| u).ok_or_else(|| Error::status(404, format!("This host does not lend the {} engine.", job.engine)))?;
            // the hub may ask the engines for answers, never manage them (security.engineGuard: false lifts this)
            let m = if job.method.is_empty() { "GET".to_string() } else { job.method.to_ascii_uppercase() };
            let managed = ["delete", "pull", "push", "create", "copy", "blobs"].iter().any(|w| { let p = job.path.to_ascii_lowercase(); p.strip_prefix("/api/").map(|r| r.starts_with(w) && r[w.len()..].chars().next().map(|c| !c.is_alphanumeric() && c != '_').unwrap_or(true)).unwrap_or(false) });
            if h.cfg.engine_guard() && (!(m == "GET" || m == "POST") || managed) { return Err(Error::status(403, "This host does not let VibeRoller manage its engines (only use them).")); }
            (base, None)
        };
        if !path_ok(&job.path) { return Err(Error::status(400, "bad path")); }
        let method = reqwest::Method::from_bytes(if job.method.is_empty() { b"GET" } else { job.method.as_bytes() }).map_err(|_| Error::status(400, "bad method"))?;
        let mut rb = h.http.request(method, format!("{}{}", base.trim_end_matches('/'), job.path)).timeout(Duration::from_secs(290));
        if let Some(ct) = job.header("content-type").filter(|s| !s.is_empty()) { rb = rb.header("content-type", ct); }
        if let Some(a) = auth { rb = rb.header("authorization", a); }
        let body = job.body_bytes()?; if !body.is_empty() { rb = rb.body(body); }
        let r = rb.send().await?;
        let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let mut hdrs = Map::new(); hdrs.insert("content-type".into(), json!(ct));
        answer(&h, &job, r, hdrs, false).await
    }.await;
    if let Err(e) = out { h.link.read().await.reply(&job.id, e.code(), &ct_json(), &err_body(&e.to_string())).await; }
}
pub fn path_ok(p: &str) -> bool {
    if !p.starts_with('/') || p.chars().any(char::is_whitespace) { return false; }
    let pathpart = p.split('?').next().unwrap_or("");
    !pathpart.contains("//") && !pathpart.split('/').any(|s| s == "..")
}
/// Hands an answer back: streamed piece by piece when it is a stream (SSE, NDJSON, plain text), else whole.
async fn answer(h: &H, job: &Job, r: reqwest::Response, hdrs: Map<String, Value>, relay_only_sse: bool) -> Result<()> {
    let status = r.status().as_u16();
    let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let streamed = if relay_only_sse { ct.contains("text/event-stream") } else { ct.contains("text/event-stream") || ct.contains("ndjson") || ct.contains("text/plain") };
    if !streamed { let b = r.bytes().await?; h.link.read().await.reply(&job.id, status, &hdrs, &b).await; return Ok(()); }
    let link = h.link.read().await;
    if !link.stream(&job.id, json!({ "status": status, "headers": hdrs })).await { return Ok(()); } // the hub no longer waits for it
    let mut s = r.bytes_stream();
    let (mut held, mut last) = (Vec::<u8>::new(), Instant::now());
    loop {
        match tokio::time::timeout(Duration::from_millis(60), s.next()).await {
            Ok(Some(Ok(chunk))) => { held.extend_from_slice(&chunk); }
            Ok(Some(Err(e))) => { let _ = link.stream(&job.id, json!({ "done": true, "error": e.to_string() })).await; return Ok(()); }
            Ok(None) => break,
            Err(_) => {} // nothing new for 60 ms: send what is held
        }
        if !held.is_empty() && (held.len() >= 4096 || last.elapsed() >= Duration::from_millis(60)) {
            let chunk = std::mem::take(&mut held); last = Instant::now();
            if !link.stream(&job.id, json!({ "chunk": pgm_core_internal::b64(&chunk) })).await { return Ok(()); }
        }
    }
    if !held.is_empty() { link.stream(&job.id, json!({ "chunk": pgm_core_internal::b64(&held) })).await; }
    link.stream(&job.id, json!({ "done": true })).await;
    Ok(())
}

/// Direct computer access: the five PC tools on pc.folder, never above it.
async fn pc_job(h: H, job: Job) {
    let out = (|| -> std::result::Result<String, String> {
        let root = h.cfg.pc_folder().ok_or("This host offers no computer access (pc.folder is not set).")?;
        let name = job.path.trim_start_matches('/');
        let args: Value = if job.body.is_empty() { json!({}) } else { serde_json::from_slice(&job.body_bytes().map_err(|e| e.to_string())?).map_err(|e| e.to_string())? };
        let rel = args["path"].as_str().unwrap_or("").replace('\\', "/");
        let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
        if segs.contains(&"..") { return Err("Paths cannot go above the shared folder.".into()); }
        let full = segs.iter().fold(root.clone(), |p, s| p.join(s));
        if !full.starts_with(&root) { return Err("Paths cannot go above the shared folder.".into()); }
        let rel = segs.join("/");
        match name {
            "list" => { let mut v: Vec<String> = std::fs::read_dir(&full).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).map(|e| { let n = e.file_name().to_string_lossy().to_string(); if e.path().is_dir() { n + "/" } else { n } }).collect(); v.sort(); Ok(if v.is_empty() { "(empty folder)".into() } else { v.join("\n") }) }
            "read" => { if std::fs::metadata(&full).map_err(|e| e.to_string())?.len() > 5_000_000 { return Err("That file is larger than 5 MB.".into()); } std::fs::read_to_string(&full).map_err(|e| e.to_string()) }
            "write" => { if rel.is_empty() { return Err("Missing file name.".into()); } let c = args["content"].as_str().unwrap_or(""); if let Some(p) = full.parent() { std::fs::create_dir_all(p).map_err(|e| e.to_string())?; } std::fs::write(&full, c).map_err(|e| e.to_string())?; Ok(format!("Wrote {rel} ({} chars)", c.chars().count())) }
            "delete" => { if rel.is_empty() { return Err("Refusing to delete the whole folder.".into()); } if full.is_dir() { std::fs::remove_dir_all(&full) } else { std::fs::remove_file(&full) }.or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(|e| e.to_string())?; Ok(format!("Deleted {rel}")) }
            "mkdir" => { std::fs::create_dir_all(&full).map_err(|e| e.to_string())?; Ok(format!("Created folder {rel}")) }
            n => Err(format!("Unknown tool {n}")),
        }
    })();
    let body = match out { Ok(r) => json!({ "ok": true, "result": r }), Err(e) => json!({ "ok": false, "error": e }) };
    h.link.read().await.reply(&job.id, 200, &ct_json(), body.to_string().as_bytes()).await;
}

/// Updates of this binary from the hub (on by default; "autoUpdate": false switches them off): the signed manifest for
/// this program and target, the file checked against it, a --selftest of the new one, the swap, a restart (exit 3).
pub async fn check_update(h: H, now: bool, exit: tokio::sync::mpsc::Sender<i32>) {
    if (!h.cfg.auto_update() && !now) || h.folder.trial().exists() { return; }
    if now { if h.busy() { return; } } else if !h.quiet(h.cfg.update_quiet_ms()) { return; }
    { let mut u = h.updating.lock().unwrap_or_else(|e| e.into_inner()); if *u { return; } *u = true; }
    let r = do_update(&h).await;
    *h.updating.lock().unwrap_or_else(|e| e.into_inner()) = false;
    match r { Ok(true) => { h.write_status_now(); let _ = exit.send(3).await; } Ok(false) => {} Err(e) => note_update(&h, &format!("update skipped: {e}")) }
}
async fn do_update(h: &H) -> Result<bool> {
    let link = h.link.read().await;
    if link.key.is_none() { return Ok(false); }
    let r = link.get(&format!("/node/update/bin?app=vr-host&target={TARGET}")).timeout(Duration::from_secs(20)).send().await?;
    if !r.status().is_success() { return Ok(false); } // an older hub hands out no binaries
    let signed: pgm_dist_internal::update::Signed = r.json().await?;
    let m = pgm_dist_internal::update::verify(&signed, Some(link.token()))?;
    let bad = std::fs::read_to_string(h.folder.bad()).unwrap_or_default();
    if !bad.trim().is_empty() && (bad.trim() == m.commit || bad.trim() == m.version) { return Ok(false); }
    let exe = std::env::current_exe()?;
    let name = exe.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let Some(f) = m.files.iter().find(|f| f.name == name || f.name.trim_end_matches(".exe") == name.trim_end_matches(".exe")) else { return Ok(false) };
    if pgm_core_internal::crypto::sha256_hex(&std::fs::read(&exe)?) == f.sha256 { return Ok(false); } // already this one
    let d = link.get(&format!("/node/update/bin/file?app=vr-host&target={TARGET}&name={}", pgm_core_internal::text::urlencode(&f.name))).timeout(Duration::from_secs(300)).send().await?;
    if !d.status().is_success() { return Err(Error::msg(format!("the download answered {}", d.status().as_u16()))); }
    let bytes = d.bytes().await?.to_vec();
    if pgm_core_internal::crypto::sha256_hex(&bytes) != f.sha256 { return Err(Error::msg(format!("{} did not match the signed release", f.name))); }
    let stage = h.folder.f(&format!(".update-{name}"));
    std::fs::write(&stage, &bytes)?;
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o755))?; }
    let st = pgm_dist_internal::update::selftest(&stage, &["--config", &h.folder.config.display().to_string()]);
    let _ = std::fs::remove_file(&stage);
    st?;
    if h.busy() { return Ok(false); } // a job came in meanwhile: next time
    pgm_dist_internal::update::swap_in(&exe, &bytes, &f.sha256)?;
    let _ = std::fs::write(h.folder.trial(), json!({ "bin": true, "from": VERSION, "to": if m.version.is_empty() { m.commit.clone() } else { m.version.clone() }, "starts": 0 }).to_string());
    note_update(h, &format!("updated from the hub to {} ({}): restarting on it", m.version, f.name));
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn engine_paths() {
        assert!(super::path_ok("/api/generate"));
        assert!(super::path_ok("/v1/models?x=1"));
        assert!(!super::path_ok("api"));
        assert!(!super::path_ok("/a/../b"));
        assert!(!super::path_ok("//evil"));
        assert!(!super::path_ok("/a b"));
    }
    #[test]
    fn ollama_tags_give_the_model_names() {
        let t = serde_json::json!({ "models": [{ "name": "llama3.2:latest", "model": "llama3.2:latest" }, { "model": "qwen3:8b" }, { "size": 1 }] });
        assert_eq!(super::ollama_models(&t), vec!["llama3.2:latest".to_string(), "qwen3:8b".to_string()]);
        assert!(super::ollama_models(&serde_json::json!({ "error": "x" })).is_empty());
        for (v, want) in [("", "http://127.0.0.1:11434"), ("0.0.0.0", "http://127.0.0.1:11434"), ("0.0.0.0:11435", "http://127.0.0.1:11435"), ("127.0.0.1:5000", "http://127.0.0.1:5000"), ("http://box.lan:11434/", "http://box.lan:11434"), ("localhost", "http://localhost:11434"), ("[::]:11434", "http://127.0.0.1:11434")] {
            assert_eq!(super::ollama_url_of(v), want, "{v}");
        }
    }
}
