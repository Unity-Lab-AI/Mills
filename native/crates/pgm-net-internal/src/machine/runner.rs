//! The machine runner (plans/linked-machines.md 2.2, 4.4, 7.2, T-311): what a linked machine does all day, in the library, so
//! vr-host, vr-desktop and `jcodepgm machine` reuse it and no app has its own copy.
//!
//! - [`link`]: the device-code link (start, show the code, poll until the person approves on the web), the per-device key, the
//!   token and a link record (`link.json`: the account and the machine id, written once, from the approval).
//! - [`Runner::run`]: the poll loop against `/api/machine/poll` with backoff. Each job the hub hands out is verified by the
//!   machine itself ([`super::sign::Verifier`], pinned to the hub's key), run through [`Machine::handle`] (scope, limits, the
//!   file tools through the one canonicaliser, audit) and answered with a reply signed by the device key.
//! - Stopped (the `KILL` file): no poll, no jobs, jobs ended ([`super::kill`]). Revoked (401 "revoked"): the token and key
//!   are wiped ([`super::device::LinkStore::handle_answer`]), jobs ended, the loop returns [`Ended::Revoked`].
//! - The hub's key is pinned the first time a poll answer carries it (`hub-key` in the state folder); a different key
//!   later is not taken (the pinned one stays and the new requests are refused).
use super::device::{DeviceKey, LinkStore};
use super::exec::{Link, Machine};
use super::kill::StopHooks;
use super::sign::Verifier;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const LINK_FILE: &str = "link.json";
pub const HUB_KEY_FILE: &str = "hub-key";

#[derive(Clone, Debug)]
pub struct Options {
    /// the hub's address, e.g. `https://viberoller.example` or `http://127.0.0.1:7777`
    pub hub: String,
    /// the machine's state folder (token, device key, scope, audit log, KILL)
    pub state_dir: PathBuf,
    pub app: String,
    pub app_version: String,
    /// `persistent` (a service) or `while open` (a desktop app)
    pub mode: String,
    /// how long one poll is held by the hub (seconds, up to 25)
    pub wait_secs: u64,
}
impl Options {
    pub fn new(hub: &str, state_dir: &Path, app: &str) -> Options { Options { hub: hub.trim_end_matches('/').to_string(), state_dir: state_dir.to_path_buf(), app: app.to_string(), app_version: String::new(), mode: "persistent".into(), wait_secs: 20 } }
}

/// What the approval left on the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRecord { pub owner: String, pub machine: String, pub hub: String }
impl LinkRecord {
    pub fn load(dir: &Path) -> Option<LinkRecord> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(LINK_FILE)).ok()?).ok()?;
        Some(LinkRecord { owner: v["owner"].as_str()?.to_string(), machine: v["machine"].as_str()?.to_string(), hub: v["hub"].as_str().unwrap_or("").to_string() })
    }
    fn save(&self, dir: &Path) -> std::io::Result<()> {
        let f = dir.join(LINK_FILE);
        std::fs::write(&f, json!({ "owner": self.owner, "machine": self.machine, "hub": self.hub }).to_string())?;
        pgm_core_internal::fs::private(&f);
        Ok(())
    }
}

fn client() -> Result<reqwest::Client, String> { crate::client::http_client(Duration::from_secs(15)).map_err(|e| e.to_string()) }

/// Links this machine by device code. `show` gets the user code to display ("approve BBBB-CCCC on the web, signed in as yourself").
/// `folder` is the default scope's one folder (written to `scope.json` when there is none yet). Returns the link record.
pub async fn link(o: &Options, folder: &str, device: &str, show: &dyn Fn(&str)) -> Result<LinkRecord, String> {
    let store = LinkStore::new(&o.state_dir).map_err(|e| e.to_string())?;
    let key = store.load_or_create_key().map_err(|e| e.to_string())?;
    let mid = {
        let f = o.state_dir.join("machine-id");
        match std::fs::read_to_string(&f).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) { Some(s) => s, None => { let s = format!("mach-{}", pgm_core_internal::crypto::random_hex(8)); let _ = std::fs::write(&f, &s); s } }
    };
    let http = client()?;
    let start: Value = http.post(format!("{}/api/device/start", o.hub)).json(&json!({ "client": "vr-machine", "device": device, "publicKey": key.public_hex(), "machineId": mid, "os": std::env::consts::OS, "app": o.app, "appVersion": o.app_version, "mode": o.mode, "folder": folder }))
        .send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let (Some(code), Some(user_code)) = (start["deviceCode"].as_str(), start["userCode"].as_str()) else { return Err(start["error"].as_str().unwrap_or("the hub did not start a link").to_string()) };
    show(user_code);
    let interval = start["interval"].as_u64().unwrap_or(5).max(1);
    let deadline = std::time::Instant::now() + Duration::from_secs(start["expiresIn"].as_u64().unwrap_or(600));
    loop {
        let r = http.post(format!("{}/api/device/poll", o.hub)).json(&json!({ "deviceCode": code })).send().await.map_err(|e| e.to_string())?;
        let st = r.status().as_u16();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        match st {
            200 => {
                let token = v["token"].as_str().ok_or("the hub approved without a token")?;
                let rec = LinkRecord { owner: v["username"].as_str().unwrap_or("").to_string(), machine: v["machineId"].as_str().or(v["deviceId"].as_str()).unwrap_or("").to_string(), hub: o.hub.clone() };
                if rec.owner.is_empty() || rec.machine.is_empty() { return Err("the hub's answer named no account".into()); }
                store.save_token(token).map_err(|e| e.to_string())?;
                rec.save(&o.state_dir).map_err(|e| e.to_string())?;
                let sf = o.state_dir.join("scope.json");
                if !sf.exists() && !folder.is_empty() {
                    let _ = std::fs::write(&sf, json!({ "v": 1, "preset": "folder", "paths": [{ "path": folder, "access": "rw" }] }).to_string());
                    pgm_core_internal::fs::private(&sf);
                }
                return Ok(rec);
            }
            403 => return Err("the person refused the link".into()),
            410 => return Err("the link request expired".into()),
            428 | 429 => {}
            s => return Err(format!("the hub answered {s} while linking: {}", v["error"].as_str().unwrap_or(""))),
        }
        if std::time::Instant::now() > deadline { return Err("nobody approved the link in time".into()); }
        tokio::time::sleep(Duration::from_secs(interval + 1)).await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended { Revoked, Cancelled }

struct Hooks { machine: Arc<Machine>, dropped: Arc<AtomicBool> }
impl StopHooks for Hooks {
    fn stop_jobs(&self) { self.machine.stop_jobs(); }
    fn drop_link(&self) { self.dropped.store(true, Ordering::SeqCst); self.machine.set_link(None); }
}

pub struct Runner {
    machine: Arc<Machine>,
    store: LinkStore,
    key: DeviceKey,
    o: Options,
    rec: LinkRecord,
    http: reqwest::Client,
    cancel: AtomicBool,
    wake: tokio::sync::Notify,
    pinned: Mutex<Option<String>>,
    log: Box<dyn Fn(&str) + Send + Sync>,
}

impl Runner {
    /// Opens a linked machine's state (`link.json`, the token, the device key). Err when this machine is not linked.
    pub fn open(mut o: Options) -> Result<Arc<Runner>, String> {
        let store = LinkStore::new(&o.state_dir).map_err(|e| e.to_string())?;
        let rec = LinkRecord::load(&o.state_dir).ok_or("this machine is not linked yet (no link record)")?;
        if o.hub.is_empty() { o.hub = rec.hub.clone(); }
        if o.hub.is_empty() { return Err("no hub address (give one, or link again)".into()); }
        if !store.linked() { return Err("this machine has no token: link it first".into()); }
        let key = store.load_or_create_key().map_err(|e| e.to_string())?;
        let machine = Arc::new(Machine::open(&o.state_dir, &rec.machine, &o.app).map_err(|e| e.to_string())?);
        let pinned = std::fs::read_to_string(o.state_dir.join(HUB_KEY_FILE)).ok().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let r = Runner { machine, store, key, rec, http: client()?, cancel: AtomicBool::new(false), wake: tokio::sync::Notify::new(), pinned: Mutex::new(pinned), log: Box::new(|m| eprintln!("[machine] {m}")), o };
        r.pin_verifier(None);
        Ok(Arc::new(r))
    }
    pub fn machine(&self) -> &Arc<Machine> { &self.machine }
    pub fn record(&self) -> &LinkRecord { &self.rec }
    pub fn linked(&self) -> bool { self.store.linked() }
    pub fn set_log(&mut self, f: Box<dyn Fn(&str) + Send + Sync>) { self.log = f; }
    /// Ends [`Runner::run`] (the link stays: the machine is just not listening).
    pub fn cancel(&self) { self.cancel.store(true, Ordering::SeqCst); self.wake.notify_waiters(); }

    fn say(&self, m: &str) { (self.log)(m); }

    /// The hub's key: the first one announced is pinned (file `hub-key`); a different one later is refused, not taken.
    fn pin_verifier(&self, announced: Option<&str>) {
        let mut p = self.pinned.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = announced.map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()) {
            match p.as_deref() {
                None => {
                    if Verifier::new(&a, &self.rec.owner, &self.rec.machine).is_ok() {
                        let f = self.o.state_dir.join(HUB_KEY_FILE);
                        let _ = std::fs::write(&f, format!("{a}\n"));
                        pgm_core_internal::fs::private(&f);
                        self.say(&format!("pinned the hub's signing key {}", a.chars().take(16).collect::<String>()));
                        *p = Some(a);
                    }
                }
                Some(old) if old != a => { self.say(&format!("the hub announced a different signing key ({}) than the pinned one: keeping the pinned one; delete {} to accept it on purpose", a.chars().take(16).collect::<String>(), self.o.state_dir.join(HUB_KEY_FILE).display())); return; }
                Some(_) => return,
            }
        }
        if !self.machine.has_verifier() {
            if let Some(k) = p.as_deref() { if let Ok(v) = Verifier::new(k, &self.rec.owner, &self.rec.machine) { self.machine.set_verifier(Some(Arc::new(v))); } }
        }
    }

    async fn post(&self, path: &str, body: &Value, timeout: Duration) -> Result<(u16, Value), String> {
        let tok = self.store.token().ok_or("no token")?;
        let r = self.http.post(format!("{}{path}", self.o.hub)).bearer_auth(tok).json(body).timeout(timeout).send().await.map_err(|e| e.to_string())?;
        let st = r.status().as_u16();
        Ok((st, r.json::<Value>().await.unwrap_or(Value::Null)))
    }

    /// Called with every answer: a 401 "revoked" wipes the link and ends the jobs. True = revoked.
    fn revoked(&self, st: u16, v: &Value) -> bool {
        if self.store.handle_answer(st, v) {
            self.machine.stop_jobs();
            self.machine.set_link(None);
            self.say("the hub says this machine was unlinked: the token and key are wiped and its jobs ended");
            true
        } else { false }
    }

    /// The loop. Returns when the machine is unlinked ([`Ended::Revoked`]) or [`Runner::cancel`] is called.
    pub async fn run(self: Arc<Self>) -> Ended {
        let dropped = Arc::new(AtomicBool::new(false));
        let _watch = self.machine.kill().clone().watch(Arc::new(Hooks { machine: self.machine.clone(), dropped: dropped.clone() }), super::kill::WATCH_INTERVAL);
        self.machine.set_link(Some(Link { base: self.o.hub.clone(), token: self.store.token().unwrap_or_default(), http: self.http.clone() }));
        let gate = Arc::new(tokio::sync::Semaphore::new(16));
        let mut backoff = 1u64;
        loop {
            if self.cancel.load(Ordering::SeqCst) { return Ended::Cancelled; }
            // stopped on the machine: no poll, no link, no jobs, until the person starts access again here
            if self.machine.kill().is_stopped() {
                self.machine.set_link(None);
                tokio::select! { _ = tokio::time::sleep(Duration::from_millis(500)) => {}, _ = self.wake.notified() => {} }
                continue;
            }
            if self.machine.link().is_none() { self.machine.set_link(Some(Link { base: self.o.hub.clone(), token: self.store.token().unwrap_or_default(), http: self.http.clone() })); }
            self.machine.tick();
            self.machine.reload_scope(false);
            let wait = self.o.wait_secs.min(25);
            let body = json!({ "wait": wait, "scope": self.machine.summary(), "mode": self.o.mode, "tools": self.machine.tools() });
            let res = tokio::select! { r = self.post("/api/machine/poll", &body, Duration::from_secs(wait + 20)) => r, _ = self.wake.notified() => continue };
            let (st, v) = match res { Ok(x) => x, Err(e) => { self.say(&format!("the hub is not reachable ({e}); trying again in {backoff} s")); tokio::time::sleep(Duration::from_secs(backoff)).await; backoff = (backoff * 2).min(30); continue; } };
            if self.revoked(st, &v) { return Ended::Revoked; }
            if st != 200 { self.say(&format!("poll answered {st}; trying again in {backoff} s")); tokio::time::sleep(Duration::from_secs(backoff)).await; backoff = (backoff * 2).min(30); continue; }
            backoff = 1;
            if let Some(k) = v["hubKey"].as_str() { self.pin_verifier(Some(k)); }
            if v["owner"].as_str().map(|o| o != self.rec.owner).unwrap_or(false) { self.say("the hub names another owner than this machine's link: ignoring its jobs"); tokio::time::sleep(Duration::from_secs(5)).await; continue; }
            for job in v["jobs"].as_array().cloned().unwrap_or_default() {
                let Ok(permit) = gate.clone().acquire_owned().await else { continue };
                let me = self.clone();
                tokio::spawn(async move { me.one(&job).await; drop(permit); });
            }
        }
    }

    async fn one(&self, job: &Value) {
        let (id, tool, body, sig) = (job["id"].as_str().unwrap_or(""), job["tool"].as_str().unwrap_or(""), job["body"].as_str().unwrap_or(""), job["sig"].as_str().unwrap_or(""));
        if id.is_empty() { return; }
        let out = self.machine.handle(tool, body.as_bytes(), sig).await;
        let text = out.to_string();
        let sig = self.key.sign_reply(&self.rec.machine, id, text.as_bytes());
        for attempt in 0..3u64 {
            match self.post("/api/machine/reply", &json!({ "requestId": id, "body": text, "sig": sig }), Duration::from_secs(60)).await {
                Ok((st, v)) => { if self.revoked(st, &v) { return; } if st < 500 { return; } }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(300 << attempt)).await;
        }
    }
}
