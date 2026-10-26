//! PGM_Sandbox_Internal: throw-away container copies of a pre-set-up base image (board T-303; first user: reverse
//! engineering, REA). One [`Manager`] per program (the hub, the AI host, a native app, Jcode PGM) over a
//! [`Runtime`] (Podman or Docker command line, or a fake in tests).
//!
//! The rules (Sponge, 2026-10-25):
//! - every request gets its own copy of the base image, started on demand (`pgm-re-<id>`);
//! - at most `max_concurrent` (default 3) run at once; more wait in a FIFO queue and start as slots free;
//! - a copy stays until its owner (or their agent) says it is no longer needed; then the container AND its exchange
//!   folder are deleted entirely (semi-shared, non-persistent). An admin may stop any copy;
//! - nothing is ever silently kept forever or silently killed: a copy idle for `idle_warn_hours` is only flagged;
//! - data goes in and out through the exchange folder (`put` -> /work/in, `get` <- /work/out), never a host mount;
//! - after a restart [`Manager::reconcile`] adopts the running `pgm-re-*` copies it knows (a `meta.json` in their
//!   exchange folder) and lists the rest as orphans; it never kills anything.
mod exchange;
pub mod mcp;
mod runtime;

pub use exchange::Area;
pub use mcp::{McpError, McpSession};
pub use runtime::{Cli, ExecEnd, Flavour, Found, Proc, RunSpec, Runtime, Stream};

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// A refusal or failure, with the HTTP status a hub answers it with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error { pub status: u16, pub msg: String }
impl Error { pub fn new(status: u16, msg: impl Into<String>) -> Error { Error { status, msg: msg.into() } } }
impl std::fmt::Display for Error { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.msg) } }
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Who asks: their account name and whether they are an admin. A person's agent is the person.
#[derive(Clone, Debug)]
pub struct Actor { pub user: String, pub admin: bool }
impl Actor { pub fn new(user: &str, admin: bool) -> Actor { Actor { user: user.to_string(), admin } } }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State { Queued, Starting, Running, Stopping, Gone }

/// One copy as people see it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sandbox {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub purpose: String,
    pub image: String,
    pub state: State,
    pub network: bool,
    /// when it was asked for / last used (exec, put, get) / started (ms since 1970)
    pub created: u64,
    pub last_active: u64,
    pub started: Option<u64>,
    /// 1 = next in line (only while queued)
    pub queue_position: Option<usize>,
    pub idle_hours: f64,
    /// idle for at least `idle_warn_hours`: a flag for people, never a reason to kill it
    pub idle: bool,
    pub adopted: bool,
    /// why it is gone without being stopped (it did not start, it ended by itself)
    pub error: Option<String>,
}

/// A container or folder this manager does not know (never touched until an admin stops it by id).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Orphan { pub id: String, pub kind: String, pub name: String, pub reason: String }

#[derive(Clone, Debug, Default)]
pub struct StartOpts {
    /// give this copy a network (default: none)
    pub network: bool,
    /// another image than the configured one (the hub never passes this)
    pub image: Option<String>,
}

/// What to run inside.
#[derive(Clone, Debug)]
pub enum Cmd { Shell(String), Argv(Vec<String>) }
impl Cmd {
    /// From a JSON value: a string (run by `sh -c`) or an array of strings (run as given).
    pub fn from_json(v: &Value) -> Option<Cmd> {
        match v {
            Value::String(s) if !s.trim().is_empty() => Some(Cmd::Shell(s.clone())),
            Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_string) => Some(Cmd::Argv(a.iter().filter_map(|x| x.as_str().map(String::from)).collect())),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult { pub code: Option<i32>, pub stdout: String, pub stderr: String, pub timed_out: bool, pub truncated: bool, pub ms: u64 }

#[derive(Clone, Debug, Serialize)]
pub struct FileInfo { pub area: String, pub path: String, pub size: u64 }

#[derive(Clone, Debug)]
pub struct Config {
    pub image: String,
    pub max_concurrent: usize,
    pub idle_warn_hours: f64,
    /// container names are `<prefix><id>`; the id is 8 hex characters
    pub name_prefix: String,
    /// where the exchange folders live (`<root>/<id>/{in,out,meta.json}`)
    pub root: PathBuf,
    pub cpus: String,
    pub memory: String,
    pub pids_limit: u32,
    /// capabilities kept after `--cap-drop ALL`: SYS_PTRACE for lldb / strace / frida attaching to a process
    pub cap_add: Vec<String>,
    pub allow_network: bool,
    pub max_file_bytes: u64,
    pub max_queued_per_owner: usize,
    pub exec_timeout_default: u64,
    pub exec_timeout_max: u64,
    pub max_output_bytes: usize,
    /// the MCP server inside the image (run with `exec -i`): REA is `node /opt/rea/node_modules/rea-agents/scripts/rea.mjs mcp`
    pub mcp_command: Vec<String>,
    /// the skill folder inside the image (SKILL.md and references/)
    pub skill_dir: String,
    /// a tool result longer than this (bytes of JSON) is saved to /work/out/mcp-results/ and a head is returned
    pub mcp_result_cap: usize,
}
impl Config {
    pub fn new(root: impl Into<PathBuf>, image: &str) -> Config {
        Config { image: image.to_string(), max_concurrent: 3, idle_warn_hours: 4.0, name_prefix: "pgm-re-".into(), root: root.into(), cpus: "2".into(), memory: "4g".into(), pids_limit: 1024,
            cap_add: vec!["SYS_PTRACE".into()], allow_network: true, max_file_bytes: 24 << 20, max_queued_per_owner: 10, exec_timeout_default: 120, exec_timeout_max: 3600, max_output_bytes: 1 << 20,
            mcp_command: ["/opt/node/bin/node", "/opt/rea/node_modules/rea-agents/scripts/rea.mjs", "mcp"].iter().map(|s| s.to_string()).collect(), skill_dir: "/opt/rea/skill/reverse-engineer-anything".into(), mcp_result_cap: 40_000 }
    }
    /// Defaults overlaid with a `re: { image, maxConcurrent, idleWarnHours, runtime, … }` config object.
    pub fn from_json(root: impl Into<PathBuf>, v: &Value) -> Config {
        let mut c = Config::new(root, v["image"].as_str().filter(|s| !s.is_empty()).unwrap_or("localhost/pgm-re-base:1.0.0"));
        if let Some(n) = v["maxConcurrent"].as_u64().filter(|n| *n >= 1) { c.max_concurrent = n.min(64) as usize; }
        if let Some(n) = v["idleWarnHours"].as_f64().filter(|n| *n > 0.0) { c.idle_warn_hours = n; }
        if let Some(s) = v["namePrefix"].as_str().filter(|s| !s.is_empty() && s.len() < 40 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) { c.name_prefix = s.to_string(); }
        if let Some(s) = v["cpus"].as_str().map(String::from).or(v["cpus"].as_f64().map(|n| n.to_string())) { c.cpus = s; }
        if let Some(s) = v["memory"].as_str() { c.memory = s.to_string(); }
        if let Some(n) = v["pidsLimit"].as_u64() { c.pids_limit = n as u32; }
        if let Some(a) = v["capAdd"].as_array() { c.cap_add = a.iter().filter_map(|x| x.as_str()).filter(|s| s.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')).map(String::from).collect(); }
        if let Some(b) = v["allowNetwork"].as_bool() { c.allow_network = b; }
        if let Some(n) = v["maxFileMb"].as_u64().filter(|n| *n >= 1) { c.max_file_bytes = n.min(30) << 20; }
        if let Some(n) = v["maxQueuedPerOwner"].as_u64().filter(|n| *n >= 1) { c.max_queued_per_owner = n as usize; }
        if let Some(n) = v["execTimeoutSec"].as_u64().filter(|n| *n >= 1) { c.exec_timeout_default = n; }
        if let Some(a) = v["mcpCommand"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).filter(|a| !a.is_empty()) { c.mcp_command = a; }
        if let Some(d) = v["skillDir"].as_str().filter(|d| d.starts_with('/') && !d.contains("..")) { c.skill_dir = d.trim_end_matches('/').to_string(); }
        if let Some(n) = v["mcpResultCap"].as_u64().filter(|n| *n >= 1000) { c.mcp_result_cap = n as usize; }
        c
    }
}

struct Item { sb: Sandbox, saved: u64 }
#[derive(Default)]
struct Inner { items: Vec<Item>, gone: VecDeque<Sandbox>, orphans: Vec<Orphan> }
/// One MCP session per running copy (opened on first use, closed when the copy is stopped or gone).
#[derive(Default)]
struct McpPool { sessions: Mutex<HashMap<String, Arc<McpSession>>>, opening: Mutex<()> }

pub struct Manager {
    rt: Box<dyn Runtime>,
    cfg: Config,
    inner: Mutex<Inner>,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
    mcp: McpPool,
}

const KEEP_GONE: usize = 50;
/// `timeout` inside the copy ends a runaway command cleanly (exit 124) before the host side has to kill the client
const WRAP: &str = r#"if command -v timeout >/dev/null 2>&1; then exec timeout -k 2 "$0" sh -c "$1"; else exec sh -c "$1"; fi"#;

fn is_id(s: &str) -> bool { s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }

impl Manager {
    pub fn new(rt: Box<dyn Runtime>, cfg: Config) -> Manager { Manager { rt, cfg, inner: Mutex::default(), clock: Box::new(pgm_core_internal::now_ms), mcp: McpPool::default() } }
    /// A manager with its own clock (tests).
    pub fn with_clock(rt: Box<dyn Runtime>, cfg: Config, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Manager { Manager { rt, cfg, inner: Mutex::default(), clock: Box::new(clock), mcp: McpPool::default() } }
    pub fn config(&self) -> &Config { &self.cfg }
    pub fn runtime_name(&self) -> &str { self.rt.name() }
    fn now(&self) -> u64 { (self.clock)() }
    fn lock(&self) -> MutexGuard<'_, Inner> { self.inner.lock().unwrap_or_else(|e| e.into_inner()) }
    fn dir(&self, id: &str) -> PathBuf { self.cfg.root.join(id) }
    fn name(&self, id: &str) -> String { format!("{}{}", self.cfg.name_prefix, id) }

    /// Whether the engine and the image are there (Err = what to tell people).
    pub fn ready(&self) -> std::result::Result<(), String> {
        self.rt.available()?;
        if !self.rt.image_present(&self.cfg.image) { return Err(format!("The base image {} is not here yet.", self.cfg.image)); }
        Ok(())
    }

    fn view(&self, inner: &Inner, sb: &Sandbox) -> Sandbox {
        let mut s = sb.clone();
        if s.state == State::Queued { s.queue_position = Some(inner.items.iter().filter(|i| i.sb.state == State::Queued).position(|i| i.sb.id == s.id).map(|p| p + 1).unwrap_or(1)); }
        if s.state == State::Running {
            let h = self.now().saturating_sub(s.last_active) as f64 / 3_600_000.0;
            s.idle_hours = (h * 100.0).round() / 100.0;
            s.idle = h >= self.cfg.idle_warn_hours;
        }
        s
    }

    // ---- start and the queue

    /// Ask for a copy. It starts at once when fewer than `max_concurrent` run, else it is queued (state `queued`, with
    /// its place in line); a slot freeing starts the next in line, first come first served.
    pub fn start(&self, who: &Actor, purpose: &str, o: &StartOpts) -> Result<Sandbox> {
        if o.network && !self.cfg.allow_network { return Err(Error::new(403, "Copies with a network are switched off on this hub.")); }
        let image = o.image.clone().unwrap_or_else(|| self.cfg.image.clone());
        let id = {
            let mut g = self.lock();
            if g.items.iter().filter(|i| i.sb.owner == who.user && i.sb.state == State::Queued).count() >= self.cfg.max_queued_per_owner { return Err(Error::new(429, format!("You already have {} copies waiting in the queue.", self.cfg.max_queued_per_owner))); }
            let mut id = pgm_core_internal::crypto::random_hex(4);
            while g.items.iter().any(|i| i.sb.id == id) || self.dir(&id).exists() { id = pgm_core_internal::crypto::random_hex(4); }
            let now = self.now();
            let purpose: String = purpose.chars().filter(|c| !c.is_control()).take(200).collect();
            g.items.push(Item { saved: 0, sb: Sandbox { name: self.name(&id), id: id.clone(), owner: who.user.clone(), purpose, image, state: State::Queued, network: o.network, created: now, last_active: now, started: None, queue_position: None, idle_hours: 0.0, idle: false, adopted: false, error: None } });
            id
        };
        self.pump();
        self.status_any(&id)
    }

    /// Start queued copies while slots are free. Called after every start / stop and by [`Manager::tick`].
    pub fn pump(&self) {
        loop {
            let next = {
                let mut g = self.lock();
                let active = g.items.iter().filter(|i| i.sb.state != State::Queued).count();
                if active >= self.cfg.max_concurrent { return; }
                match g.items.iter_mut().find(|i| i.sb.state == State::Queued) { Some(i) => { i.sb.state = State::Starting; i.sb.clone() } None => return }
            };
            self.launch(next);
        }
    }

    fn launch(&self, sb: Sandbox) {
        let dir = self.dir(&sb.id);
        let spec = RunSpec { name: sb.name.clone(), image: sb.image.clone(), network: sb.network, cpus: self.cfg.cpus.clone(), memory: self.cfg.memory.clone(), pids_limit: self.cfg.pids_limit, cap_add: self.cfg.cap_add.clone(), in_dir: dir.join("in"), out_dir: dir.join("out") };
        let made = std::fs::create_dir_all(&dir).and_then(|_| { protect(&dir); std::fs::create_dir_all(&spec.in_dir)?; std::fs::create_dir_all(&spec.out_dir)?; exchange::open_up(&spec.in_dir, true); exchange::open_up(&spec.out_dir, true); Ok(()) }).map_err(|e| format!("could not make the exchange folder: {e}"));
        let res = made.and_then(|_| self.rt.run(&spec));
        let mut g = self.lock();
        let Some(pos) = g.items.iter().position(|i| i.sb.id == sb.id) else { return };
        match res {
            Ok(()) => {
                let now = self.now();
                let i = &mut g.items[pos];
                i.sb.state = State::Running; i.sb.started = Some(now); i.sb.last_active = now; i.saved = now;
                self.save_meta(&i.sb);
            }
            Err(e) => {
                let mut it = g.items.remove(pos).sb;
                it.state = State::Gone; it.error = Some(format!("It did not start: {e}"));
                remember(&mut g, it);
                drop(g);
                self.rt.purge_dir(&dir);
            }
        }
    }

    /// The periodic housekeeping a program runs every half minute or so: copies whose container ended by itself are
    /// marked gone (and their exchange folder deleted), the queue moves on.
    pub fn tick(&self) {
        if let Ok(found) = self.rt.list(&self.cfg.name_prefix) {
            let dead: Vec<String> = { let g = self.lock(); g.items.iter().filter(|i| i.sb.state == State::Running && !found.iter().any(|f| f.running && f.name == i.sb.name)).map(|i| i.sb.id.clone()).collect() };
            for id in dead {
                let removed = { let mut g = self.lock(); g.items.iter().position(|i| i.sb.id == id && i.sb.state == State::Running).map(|p| { let mut s = g.items.remove(p).sb; s.state = State::Gone; s.error = Some("Its container ended by itself (out of memory, or the program inside quit); its exchange folder was deleted.".into()); remember(&mut g, s.clone()); s }) };
                if removed.is_some() { self.mcp_close(&id); self.rt.purge_dir(&self.dir(&id)); }
            }
        }
        self.pump();
    }

    // ---- reading

    /// Every copy that is queued, starting, running or stopping, oldest first.
    pub fn list(&self) -> Vec<Sandbox> { let g = self.lock(); g.items.iter().map(|i| self.view(&g, &i.sb)).collect() }
    pub fn orphans(&self) -> Vec<Orphan> { self.lock().orphans.clone() }
    /// (running or starting, queued, max)
    pub fn counts(&self) -> (usize, usize, usize) { let g = self.lock(); (g.items.iter().filter(|i| i.sb.state != State::Queued).count(), g.items.iter().filter(|i| i.sb.state == State::Queued).count(), self.cfg.max_concurrent) }
    /// One copy (also a recently gone one, with why). Reading is open to everyone on the team.
    pub fn status(&self, id: &str) -> Result<Sandbox> { self.status_any(id) }
    fn status_any(&self, id: &str) -> Result<Sandbox> {
        let g = self.lock();
        if let Some(i) = g.items.iter().find(|i| i.sb.id == id) { return Ok(self.view(&g, &i.sb)); }
        g.gone.iter().find(|s| s.id == id).cloned().ok_or_else(|| Error::new(404, "No such copy (it may have been stopped)."))
    }
    /// The files in a copy's exchange folder (what it was given, what it made).
    pub fn files(&self, id: &str) -> Vec<FileInfo> {
        if !is_id(id) || !self.lock().items.iter().any(|i| i.sb.id == id) { return vec![]; }
        let mut v = vec![];
        for a in [Area::In, Area::Out] { for (p, size) in exchange::list(&self.dir(id).join(a.dir()), 200) { v.push(FileInfo { area: a.dir().into(), path: p, size }); } }
        v
    }

    // ---- control

    /// The copy `who` may use: it must be running and theirs.
    fn running_owned(&self, who: &Actor, id: &str) -> Result<Sandbox> {
        let s = { let g = self.lock(); g.items.iter().find(|i| i.sb.id == id).map(|i| i.sb.clone()) };
        let Some(s) = s else { return Err(Error::new(404, "No such copy (it may have been stopped).")) };
        if s.owner != who.user { return Err(Error::new(403, format!("That copy is {}'s: only its owner can use it. You can start your own.", s.owner))); }
        match s.state {
            State::Running => Ok(s),
            State::Queued => { let g = self.lock(); let p = self.view(&g, &s).queue_position.unwrap_or(1); Err(Error::new(409, format!("That copy is still waiting in the queue (place {p}).")))  }
            State::Starting => Err(Error::new(409, "That copy is still starting; try again in a moment.")),
            _ => Err(Error::new(409, "That copy is stopping.")),
        }
    }
    fn touch(&self, id: &str) {
        let mut g = self.lock(); let now = self.now();
        if let Some(i) = g.items.iter_mut().find(|i| i.sb.id == id) {
            i.sb.last_active = now;
            if now.saturating_sub(i.saved) > 60_000 { i.saved = now; self.save_meta(&i.sb); }
        }
    }

    /// Run a command in the copy; collects its output (each stream capped at `max_output_bytes`, `truncated` says so).
    pub fn exec(&self, who: &Actor, id: &str, cmd: &Cmd, timeout_sec: Option<u64>) -> Result<ExecResult> {
        let cap = self.cfg.max_output_bytes;
        let (mut out, mut err, mut truncated) = (Vec::<u8>::new(), Vec::<u8>::new(), false);
        let t0 = std::time::Instant::now();
        let end = self.exec_stream(who, id, cmd, timeout_sec, &mut |s, b| {
            let buf = if s == Stream::Stdout { &mut out } else { &mut err };
            let room = cap.saturating_sub(buf.len());
            if b.len() > room { truncated = true; }
            buf.extend_from_slice(&b[..b.len().min(room)]);
        })?;
        Ok(ExecResult { code: end.code, stdout: String::from_utf8_lossy(&out).to_string(), stderr: String::from_utf8_lossy(&err).to_string(), timed_out: end.timed_out || end.code == Some(124), truncated, ms: t0.elapsed().as_millis() as u64 })
    }
    /// The same, handing output to `sink` as it arrives (a native app shows it live).
    pub fn exec_stream(&self, who: &Actor, id: &str, cmd: &Cmd, timeout_sec: Option<u64>, sink: &mut dyn FnMut(Stream, &[u8])) -> Result<ExecEnd> {
        let s = self.running_owned(who, id)?;
        let secs = timeout_sec.unwrap_or(self.cfg.exec_timeout_default).clamp(1, self.cfg.exec_timeout_max);
        let (argv, deadline) = match cmd {
            Cmd::Shell(c) => (vec!["sh".to_string(), "-c".into(), WRAP.into(), secs.to_string(), c.clone()], secs + 5),
            Cmd::Argv(a) => (a.clone(), secs),
        };
        if argv.iter().any(|a| a.contains('\0')) { return Err(Error::new(400, "A command cannot contain NUL.")); }
        self.touch(id);
        let r = self.rt.exec(&s.name, &argv, Duration::from_secs(deadline), sink).map_err(|e| Error::new(502, e));
        self.touch(id);
        r
    }

    // ---- the MCP server inside the copy (REA's tools)

    /// The copy's MCP session, opened on first use (one long-lived `exec -i` per copy) and reopened if it died.
    fn mcp_session(&self, id: &str, s: &Sandbox) -> Result<Arc<McpSession>> {
        let get = || self.mcp.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(id).filter(|m| m.alive()).cloned();
        if let Some(m) = get() { return Ok(m); }
        let _one_at_a_time = self.mcp.opening.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = get() { return Ok(m); }
        let m = Arc::new(McpSession::open(&*self.rt, &s.name, &self.cfg.mcp_command, Duration::from_secs(60)).map_err(|e| Error::new(502, format!("Could not start the MCP server inside the copy: {e}")))?);
        // the copy may have been stopped while it started: do not keep a session for it
        if self.lock().items.iter().find(|i| i.sb.id == id).map(|i| i.sb.state) != Some(State::Running) { m.close(); return Err(Error::new(409, "That copy is stopping.")); }
        self.mcp.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), m.clone());
        Ok(m)
    }
    /// End the copy's MCP session (stop, or the copy is gone). Nothing is left running in the copy.
    fn mcp_close(&self, id: &str) { let m = self.mcp.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(id); if let Some(m) = m { m.close(); } }
    /// Whether the copy has an open MCP session right now (for status and tests).
    pub fn mcp_open(&self, id: &str) -> bool { self.mcp.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|m| m.alive()).unwrap_or(false) }

    /// The command line (engine, its global arguments, `exec -i`, the copy, the MCP server) a program with its own MCP client runs to
    /// reach the server in the copy (Jcode PGM adds it to the person's MCP config while the copy runs). Owner only; the engine's
    /// environment (`Cli::with_env`) is not part of the line.
    pub fn mcp_exec_line(&self, who: &Actor, id: &str) -> Result<Vec<String>> {
        let s = self.running_owned(who, id)?;
        self.rt.exec_line(&s.name, &self.cfg.mcp_command).ok_or_else(|| Error::new(501, "This engine has no command line."))
    }

    fn mcp_timeout(&self, secs: Option<u64>) -> Duration { Duration::from_secs(secs.unwrap_or(self.cfg.exec_timeout_default).clamp(1, self.cfg.exec_timeout_max)) }
    fn mcp_err(e: McpError) -> Error { match e { McpError::Timeout => Error::new(504, e.to_string()), McpError::Closed(_) => Error::new(502, e.to_string()), McpError::Rpc(..) => Error::new(422, e.to_string()) } }

    /// The tools of the MCP server in the copy: `{ count, of, tools: [{ name, description }] }`, or one tool in full
    /// (`tool`), or those matching `filter`. The server is started on the first call and kept for the copy's life.
    pub fn mcp_tools(&self, who: &Actor, id: &str, filter: &str, tool: &str) -> Result<Value> {
        let s = self.running_owned(who, id)?;
        self.touch(id);
        let m = self.mcp_session(id, &s)?;
        let all = m.tools(self.mcp_timeout(Some(60))).map_err(Self::mcp_err)?;
        let mut v = mcp::summarize(&all, filter, tool);
        if let Some(e) = v["error"].as_str() { return Err(Error::new(404, e)); }
        v["server"] = m.info["serverInfo"].clone();
        Ok(v)
    }

    /// Call one tool of the MCP server in the copy. The server's result comes back unchanged (its `content` blocks, its
    /// untrusted-content markers, `isError`) unless it is longer than `mcp_result_cap`: then the whole is saved to
    /// `/work/out/mcp-results/<tool>-<ms>.json` and what comes back is `{ truncated, bytes, savedTo, content: [head] }`.
    pub fn mcp_call(&self, who: &Actor, id: &str, tool: &str, args: &Value, timeout_sec: Option<u64>) -> Result<Value> {
        if tool.is_empty() || tool.len() > 200 { return Err(Error::new(400, "Give the tool's name (re_tools lists them).")); }
        let s = self.running_owned(who, id)?;
        self.touch(id);
        let m = self.mcp_session(id, &s)?;
        let r = m.call(tool, args, self.mcp_timeout(timeout_sec)).map_err(Self::mcp_err);
        self.touch(id);
        let r = r?;
        let Some((head, bytes)) = mcp::too_big(&r, self.cfg.mcp_result_cap) else { return Ok(r) };
        let safe: String = tool.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let name = format!("mcp-results/{safe}-{}.json", self.now());
        let saved = self.put(who, id, &format!("out/{name}"), r.to_string().as_bytes());
        Ok(json!({ "truncated": true, "bytes": bytes, "isError": r["isError"],
            "savedTo": saved.as_ref().ok().map(|(p, _)| p.clone()),
            "note": match &saved { Ok((p, _)) => format!("The result is {bytes} bytes; the first {} are here. All of it is saved as {p} in the copy (read it with re_exec, or take it out with re_get).", head.len()), Err(e) => format!("The result is {bytes} bytes and could not be saved ({}); only the first {} are here.", e.msg, head.len()) },
            "content": [{ "type": "text", "text": head }] }))
    }

    /// The skill text inside the image (REA's long workflow skill): SKILL.md by default, or a file under `references/`
    /// (`file`); the answer lists the other files. Read from the copy, never from the host.
    pub fn skill(&self, who: &Actor, id: &str, file: &str) -> Result<Value> {
        let s = self.running_owned(who, id)?;
        let file = if file.is_empty() { "SKILL.md" } else { file.trim_start_matches("./") };
        if file.len() > 120 || file.contains("..") || file.starts_with('/') || !file.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b)) { return Err(Error::new(400, "file is SKILL.md or a path like references/environments.md (see the list in the answer).")); }
        self.touch(id);
        let run = |argv: Vec<String>| -> Result<(Option<i32>, Vec<u8>)> {
            let mut out = Vec::<u8>::new();
            let end = self.rt.exec(&s.name, &argv, Duration::from_secs(20), &mut |st, b| { if st == Stream::Stdout && out.len() < 4 << 20 { out.extend_from_slice(b); } }).map_err(|e| Error::new(502, e))?;
            Ok((end.code, out))
        };
        let dir = &self.cfg.skill_dir;
        let (code, text) = run(vec!["cat".into(), format!("{dir}/{file}")])?;
        if code != Some(0) { return Err(Error::new(404, format!("No skill file {file} in the image ({dir}).{}", if file == "SKILL.md" { " This image has no skill." } else { " Call re_skill without file for the list." }))); }
        let (_, list) = run(vec!["find".into(), dir.clone(), "-type".into(), "f".into()])?;
        let mut files: Vec<String> = String::from_utf8_lossy(&list).lines().filter_map(|l| l.strip_prefix(&format!("{dir}/")).map(String::from)).collect();
        files.sort();
        let text = String::from_utf8_lossy(&text).to_string();
        Ok(json!({ "file": file, "text": text, "files": files, "note": "Read the other files only when the task needs them (re_skill with file)." }))
    }

    /// Put a file into the copy's /work/in (a path starting `out/` goes to /work/out). Refuses path escapes.
    pub fn put(&self, who: &Actor, id: &str, path: &str, bytes: &[u8]) -> Result<(String, u64)> {
        self.running_owned(who, id)?;
        if bytes.len() as u64 > self.cfg.max_file_bytes { return Err(Error::new(413, format!("That file is too big: the limit is {} MB.", self.cfg.max_file_bytes >> 20))); }
        let (area, parts) = exchange::parse(path, Area::In)?;
        let n = exchange::write(&self.dir(id).join(area.dir()), &parts, bytes)?;
        self.touch(id);
        Ok((format!("/work/{}/{}", area.dir(), parts.join("/")), n))
    }
    /// Take a file out of the copy's /work/out (a path starting `in/` reads /work/in). Refuses path escapes.
    pub fn get(&self, who: &Actor, id: &str, path: &str) -> Result<Vec<u8>> {
        let s = self.running_owned(who, id)?;
        let (area, parts) = exchange::parse(path, Area::Out)?;
        let base = self.dir(id).join(area.dir());
        let first = exchange::read(&base, &parts, self.cfg.max_file_bytes);
        let r = match first {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // a file made by the copy's own user, private to it: let the host read it, then try once more
                let mut sink = |_: Stream, _: &[u8]| {};
                let _ = self.rt.exec(&s.name, &["sh".into(), "-c".into(), "chmod -R a+rX /work/out /work/in 2>/dev/null; true".into()], Duration::from_secs(20), &mut sink);
                exchange::read(&base, &parts, self.cfg.max_file_bytes)
            }
            other => other,
        };
        self.touch(id);
        r.map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { Error::new(404, "No such file in the exchange folder.") } else if e.kind() == std::io::ErrorKind::InvalidInput { Error::new(400, e.to_string()) } else { Error::new(500, format!("could not read it: {e}")) })
    }

    /// The copy is no longer needed: its container is removed and its exchange folder deleted entirely. The owner
    /// (or their agent) may do it; an admin may stop any copy, and clear an orphan by its id.
    pub fn stop(&self, who: &Actor, id: &str) -> Result<Sandbox> {
        if !is_id(id) { return Err(Error::new(400, "Not a copy id.")); }
        let s = { let g = self.lock(); g.items.iter().find(|i| i.sb.id == id).map(|i| i.sb.clone()) };
        let Some(s) = s else {
            if let Some(o) = { let g = self.lock(); g.orphans.iter().find(|o| o.id == id).cloned() } {
                if !who.admin { return Err(Error::new(403, "Only an admin can clear an orphan.")); }
                let _ = self.rt.remove(&self.name(id));
                self.rt.purge_dir(&self.dir(id));
                self.lock().orphans.retain(|x| x.id != o.id);
                let now = self.now();
                return Ok(Sandbox { id: id.into(), name: o.name, owner: String::new(), purpose: "orphan, cleared".into(), image: String::new(), state: State::Gone, network: false, created: now, last_active: now, started: None, queue_position: None, idle_hours: 0.0, idle: false, adopted: false, error: None });
            }
            return self.status_any(id); // already gone: say so (or 404)
        };
        if s.owner != who.user && !who.admin { return Err(Error::new(403, format!("That copy is {}'s: only its owner (or an admin) can stop it.", s.owner))); }
        match s.state {
            State::Queued => {
                let mut g = self.lock();
                let Some(p) = g.items.iter().position(|i| i.sb.id == id && i.sb.state == State::Queued) else { drop(g); return self.status_any(id) };
                let mut it = g.items.remove(p).sb; it.state = State::Gone; remember(&mut g, it.clone());
                return Ok(it);
            }
            State::Starting => return Err(Error::new(409, "That copy is still starting; stop it in a moment.")),
            State::Stopping => return self.status_any(id),
            _ => {}
        }
        { let mut g = self.lock(); if let Some(i) = g.items.iter_mut().find(|i| i.sb.id == id) { i.sb.state = State::Stopping; } }
        self.mcp_close(id); // the MCP server inside ends first: nothing is left running
        // let the host delete what the copy's own user wrote (best effort), then remove the container and the folder
        let mut sink = |_: Stream, _: &[u8]| {};
        let _ = self.rt.exec(&s.name, &["sh".into(), "-c".into(), "chmod -R a+rwX /work/in /work/out 2>/dev/null; true".into()], Duration::from_secs(20), &mut sink);
        if let Err(e) = self.rt.remove(&s.name) {
            let mut g = self.lock(); if let Some(i) = g.items.iter_mut().find(|i| i.sb.id == id) { i.sb.state = State::Running; }
            return Err(Error::new(502, format!("Could not remove the container: {e}")));
        }
        self.rt.purge_dir(&self.dir(id));
        let gone = { let mut g = self.lock(); g.items.iter().position(|i| i.sb.id == id).map(|p| { let mut it = g.items.remove(p).sb; it.state = State::Gone; remember(&mut g, it.clone()); it }) };
        self.pump();
        Ok(gone.unwrap_or(Sandbox { state: State::Gone, ..s }))
    }

    // ---- after a restart

    /// Adopt the running `<prefix><id>` containers this manager made before (their `meta.json`), list the rest (other
    /// containers, folders without a copy) as orphans. Nothing is killed or deleted. Returns (adopted, orphans).
    pub fn reconcile(&self) -> Result<(usize, usize)> {
        let found = self.rt.list(&self.cfg.name_prefix).map_err(|e| Error::new(502, e))?;
        let mut adopted = 0; let mut orphans: Vec<Orphan> = vec![];
        let mut g = self.lock();
        let mut seen_ids: Vec<String> = g.items.iter().map(|i| i.sb.id.clone()).collect();
        for f in &found {
            let id = f.name[self.cfg.name_prefix.len()..].to_string();
            if !is_id(&id) { continue; } // somebody else's container that happens to share the start of the name
            if seen_ids.contains(&id) { continue; }
            seen_ids.push(id.clone());
            let meta = std::fs::read_to_string(self.dir(&id).join("meta.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
            match meta {
                Some(m) if f.running && m["owner"].as_str().is_some() => {
                    let last = m["lastActive"].as_u64().unwrap_or(0);
                    g.items.push(Item { saved: last, sb: Sandbox { id: id.clone(), name: f.name.clone(), owner: m["owner"].as_str().unwrap_or("").into(), purpose: m["purpose"].as_str().unwrap_or("").into(), image: m["image"].as_str().unwrap_or(&self.cfg.image).into(), state: State::Running, network: m["network"].as_bool().unwrap_or(false),
                        created: m["created"].as_u64().unwrap_or(last), last_active: last, started: m["started"].as_u64(), queue_position: None, idle_hours: 0.0, idle: false, adopted: true, error: None } });
                    adopted += 1;
                }
                Some(_) => orphans.push(Orphan { id, kind: "container".into(), name: f.name.clone(), reason: "its container is not running".into() }),
                None => orphans.push(Orphan { id, kind: "container".into(), name: f.name.clone(), reason: "no record of it in the exchange folders (another hub's, or lost)".into() }),
            }
        }
        if let Ok(rd) = std::fs::read_dir(&self.cfg.root) {
            for e in rd.flatten() {
                let id = e.file_name().to_string_lossy().to_string();
                if is_id(&id) && !seen_ids.contains(&id) && e.path().is_dir() { orphans.push(Orphan { id: id.clone(), kind: "folder".into(), name: id, reason: "an exchange folder without a copy".into() }); }
            }
        }
        let n = orphans.len();
        g.orphans = orphans;
        drop(g);
        self.pump();
        Ok((adopted, n))
    }

    fn save_meta(&self, s: &Sandbox) {
        let m = json!({ "id": s.id, "owner": s.owner, "purpose": s.purpose, "image": s.image, "network": s.network, "created": s.created, "started": s.started, "lastActive": s.last_active });
        let f = self.dir(&s.id).join("meta.json");
        let tmp = f.with_extension("json.tmp");
        if std::fs::write(&tmp, m.to_string()).is_ok() { let _ = std::fs::rename(&tmp, &f); }
    }
}

fn remember(g: &mut Inner, s: Sandbox) { g.gone.push_back(s); while g.gone.len() > KEEP_GONE { g.gone.pop_front(); } }

#[cfg(unix)]
fn protect(dir: &std::path::Path) { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)); }
#[cfg(not(unix))]
fn protect(_dir: &std::path::Path) {}

#[cfg(test)]
mod tests;
