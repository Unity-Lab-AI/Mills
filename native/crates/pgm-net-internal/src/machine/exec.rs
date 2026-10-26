//! The machine's tools (plans/linked-machines.md 5.4, 5.5, 6, T-311): what a request, once the hub's signature and the
//! owner are checked, may do on this computer, and nothing else.
//!
//! Every call goes through [`Machine::handle`] in the same order, so no tool can skip a step:
//! 1. the kill switch (`KILL` file): stopped = no call at all;
//! 2. the hub's signed envelope ([`super::sign::Verifier`]): bad signature, expired, repeated, another owner, an actor that is not the
//!    owner, another machine, another tool or other arguments are all refused here, by the machine itself;
//! 3. the tool must be in the scope (`Scope::has_tool`: `no-shell` has no shell tools, a read-only scope no write tools);
//! 4. the machine's own rate limits ([`super::limits`]);
//! 5. the tool, which reaches the file system only through `Scope::authorize` / `Scope::open` (the one canonicaliser, the
//!    real path, the safety floor, deny rules, `ro`), never a raw fs call on a typed path;
//! 6. one line in the audit log ([`super::audit`]) for the allow or the deny: path / command as given and the size, never contents.
//!
//! Answers are `{ "ok": true, "result": ... }` or `{ "ok": false, "error": "..." }`. The five file tools answer exactly what
//! the AI host's `pc_job` answers (text results, same words) for a folder, so `mcp__pc__*` is the same through a machine.
//!
//! The shell is ON in a folder scope with its cwd confined to the scope (advisory: a command can name any path its user can
//! reach, and the answer says so); it is never run as root; untrusted binaries belong in the RE containers (`re_*`).
use super::audit::{unix_now, AuditLog, Event, Verdict};
use super::floor::Floor;
use super::jobs::{kill_tree, shell_command, Children, JobTable, DEFAULT_WALL_SECS, JOB_TTL_SECS};
use super::kill::Kill;
use super::limits::{BurstGuard, Limiter, Limits};
use super::paths::{open_verified, NormPath, OpenMode, Resolved};
use super::scope::{KillMode, Op, Scope};
use super::sign::{Actor, Verifier};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

/// The tools this version of the library runs (a tool in the scope's list that is not here is not offered).
pub const IMPLEMENTED: &[&str] = &["machine_info", "list", "read", "write", "delete", "mkdir", "shell_run", "job_start", "job_read", "job_stop", "proc_list", "proc_kill", "re_list", "re_run", "re_logs", "re_stop", "transfer_get", "transfer_put"];

/// The largest text file `read` answers (the same 5 MB as the AI host's).
const READ_MAX: u64 = 5_000_000;

/// Reverse-engineering containers on this machine (pgm-sandbox-internal's Manager behind an adapter; the program that
/// has a container engine plugs one in with [`Machine::set_containers`]). `call` is blocking.
pub trait Containers: Send + Sync {
    /// Ok when the engine and the base image are there; Err says what is missing.
    fn available(&self) -> Result<(), String>;
    /// `re_list`, `re_run`, `re_logs`, `re_stop` for the account `who`.
    fn call(&self, tool: &str, args: &Value, who: &str) -> Result<Value, String>;
}

/// A refusal (the scope, the safety floor, a limit: audited as a deny) or a failure (an I/O error, a bad argument).
#[derive(Debug)]
pub enum Fail {
    Denied(String),
    Error(String),
}
impl Fail {
    fn err(e: impl std::fmt::Display) -> Fail { Fail::Error(e.to_string()) }
}
pub(crate) type Done = Result<(Value, u64), Fail>; // result, bytes

/// What the hub tells a transfer call (set by the runner once it is linked).
#[derive(Clone)]
pub struct Link {
    pub base: String,
    pub token: String,
    pub http: reqwest::Client,
}

struct Loaded { scope: Scope, mtime: Option<SystemTime>, max_jobs: usize, max_file_mb: u64, job_ttl: u64 }

pub struct Machine {
    pub id: String,
    pub app: String,
    state_dir: PathBuf,
    scope_file: PathBuf,
    loaded: RwLock<Loaded>,
    floor: Floor,
    limiter: Mutex<Limiter>,
    burst: BurstGuard,
    audit: AuditLog,
    kill: Arc<Kill>,
    pub jobs: Arc<JobTable>,
    containers: Mutex<Option<Arc<dyn Containers>>>,
    verifier: Mutex<Option<Arc<Verifier>>>,
    link: Mutex<Option<Link>>,
    start: std::time::Instant,
}

fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

/// `a/b/../c` the way the AI host's `pc_job` spells it in its answers: separators as `/`, `.` and empty parts dropped.
fn rel_of(given: &str) -> String { given.replace('\\', "/").split('/').filter(|s| !s.is_empty() && *s != ".").collect::<Vec<_>>().join("/") }

fn short(s: &str, n: usize) -> String { s.chars().filter(|c| !c.is_control()).take(n).collect() }

fn parse_limits(text: &str) -> (usize, u64, u64) {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let n = |p: &str, d: u64| v.pointer(p).and_then(Value::as_u64).filter(|x| *x > 0).unwrap_or(d);
    (n("/limits/concurrentJobs", 4) as usize, n("/limits/maxFileMB", 2048), n("/limits/jobTtlMin", JOB_TTL_SECS / 60) * 60)
}

impl Machine {
    /// The machine's tools over its state dir (`token`, `device-key`, `scope.json`, `machine-audit.jsonl`, `KILL`, `jobs/`).
    pub fn open(state_dir: &Path, machine_id: &str, app: &str) -> std::io::Result<Machine> {
        pgm_core_internal::fs::private_dir(state_dir)?;
        let scope_file = state_dir.join("scope.json");
        let audit_file = state_dir.join("machine-audit.jsonl");
        let floor = super::changes::state_floor(Floor::new(state_dir.join("token"), state_dir.join("device-key"), &scope_file, &audit_file), state_dir)
            .with_extra(state_dir.join("device-key")).with_extra(state_dir.join("jobs")).with_extra(state_dir.join("hub-key")).with_extra(state_dir.join("link.json"));
        let children = Children::default();
        let jobs = Arc::new(JobTable::new(&state_dir.join("jobs"), children));
        let m = Machine {
            id: machine_id.to_string(), app: app.to_string(), state_dir: state_dir.to_path_buf(), scope_file, floor,
            loaded: RwLock::new(Loaded { scope: Scope::nothing(), mtime: None, max_jobs: 4, max_file_mb: 2048, job_ttl: JOB_TTL_SECS }),
            limiter: Mutex::new(Limiter::new(Limits::default())), burst: BurstGuard::new(Limits::default().confirm_burst), audit: AuditLog::open(&audit_file)?,
            kill: Arc::new(Kill::open(state_dir)), jobs, containers: Mutex::new(None), verifier: Mutex::new(None), link: Mutex::new(None), start: std::time::Instant::now(),
        };
        m.reload_scope(true);
        Ok(m)
    }
    pub fn state_dir(&self) -> &Path { &self.state_dir }
    pub fn kill(&self) -> &Arc<Kill> { &self.kill }
    pub fn audit(&self) -> &AuditLog { &self.audit }
    pub fn set_verifier(&self, v: Option<Arc<Verifier>>) { *lk(&self.verifier) = v; }
    pub fn has_verifier(&self) -> bool { lk(&self.verifier).is_some() }
    pub fn set_link(&self, l: Option<Link>) { *lk(&self.link) = l; }
    pub fn link(&self) -> Option<Link> { lk(&self.link).clone() }
    pub fn set_containers(&self, c: Option<Arc<dyn Containers>>) { *lk(&self.containers) = c; }
    /// A job's wall clock when the call names none.
    pub fn stop_jobs(&self) -> usize { self.jobs.stop_all() }
    /// Housekeeping tick (the runner calls it every poll): finished jobs older than the keep time go.
    pub fn tick(&self) { let ttl = self.loaded.read().unwrap_or_else(|e| e.into_inner()).job_ttl; self.jobs.reap(ttl); }

    /// Re-reads `scope.json` when it changed on disk (a bad or missing file = nothing allowed). The person changes it on the
    /// machine; the AI has no path to it (safety floor).
    pub fn reload_scope(&self, force: bool) {
        let mtime = std::fs::metadata(&self.scope_file).and_then(|m| m.modified()).ok();
        { let g = self.loaded.read().unwrap_or_else(|e| e.into_inner()); if !force && g.mtime == mtime && mtime.is_some() { return; } }
        let text = std::fs::read_to_string(&self.scope_file).unwrap_or_default();
        let scope = if text.is_empty() { Scope::nothing() } else { Scope::parse(&text) }.with_floor(self.floor.clone());
        let limits = Limits::from_scope_json(&text);
        let (max_jobs, max_file_mb, job_ttl) = parse_limits(&text);
        *lk(&self.limiter) = Limiter::new(limits.clone());
        *self.loaded.write().unwrap_or_else(|e| e.into_inner()) = Loaded { scope, mtime, max_jobs, max_file_mb, job_ttl };
    }
    fn scope(&self) -> Scope { self.loaded.read().unwrap_or_else(|e| e.into_inner()).scope.clone() }
    /// The scope in words (what the card and the tray show, and what `machine_info` tells the AI).
    pub fn summary(&self) -> String { self.scope().summary(unix_now()) }
    /// The tools the AI may call now: the scope's list, minus what this version does not run, minus RE without an engine.
    pub fn tools(&self) -> Vec<&'static str> {
        let sc = self.scope();
        sc.tool_names(unix_now()).into_iter().filter(|t| IMPLEMENTED.contains(t)).collect()
    }

    /// One request, start to end (see the module docs). `sig` is the signed envelope header the hub sent with it.
    pub async fn handle(&self, tool: &str, body: &[u8], sig: &str) -> Value {
        self.reload_scope(false);
        let now = unix_now();
        if let Err(s) = self.kill.admit() { self.note("-", tool, "", Verdict::Deny, &s.to_string(), None); return json!({ "ok": false, "error": s.to_string() }); }
        let v = lk(&self.verifier).clone();
        let Some(v) = v else {
            self.note("-", tool, "", Verdict::Deny, "no hub key pinned yet", None);
            return json!({ "ok": false, "error": "This machine has not pinned the hub's signing key yet, so it refuses every request." });
        };
        let actor = match v.verify(sig, tool, body, now) {
            Ok(a) => a,
            Err(e) => { self.note("-", tool, "", Verdict::Deny, &format!("signature: {e}"), None); return json!({ "ok": false, "error": format!("This machine refused the request: {e}.") }); }
        };
        let args: Value = if body.is_empty() { json!({}) } else { match serde_json::from_slice(body) { Ok(a) => a, Err(e) => return json!({ "ok": false, "error": format!("The arguments are not JSON: {e}") }) } };
        self.run_as(&actor, tool, &args).await
    }

    /// The same, for a caller that has already established who asked (tests, and the machine's own local use).
    pub async fn run_as(&self, actor: &Actor, tool: &str, args: &Value) -> Value {
        self.reload_scope(false);
        let now = unix_now();
        let who = format!("{}/{}/{}", actor.account, if actor.session.is_empty() { "-" } else { &actor.session }, if actor.run.is_empty() { "-" } else { &actor.run });
        let given = ["path", "cmd", "command", "jobId", "pid"].iter().filter_map(|k| args.get(*k)).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).next().unwrap_or_default();
        let given = short(&given, 300);
        let sc = self.scope();
        if !sc.has_tool(tool, now) || !IMPLEMENTED.contains(&tool) {
            let why = if IMPLEMENTED.contains(&tool) { format!("the tool {tool} is not in this machine's scope ({})", sc.summary(now)) } else { format!("unknown tool {tool}") };
            self.note(&who, tool, &given, Verdict::Deny, &why, None);
            return json!({ "ok": false, "error": format!("This machine does not offer {tool}: {why}.") });
        }
        let limited = lk(&self.limiter).check_tool(tool, self.start.elapsed().as_millis() as u64);
        if let Err(r) = limited { self.note(&who, tool, &given, Verdict::Deny, &r.to_string(), None); return json!({ "ok": false, "error": r.to_string() }); }
        if matches!(tool, "write" | "delete" | "mkdir") && self.burst.note_change(self.start.elapsed().as_millis() as u64) {
            let why = "a burst of changes (more than the scope's confirm threshold in a minute): the machine asks its owner to confirm, and no one is here to";
            self.note(&who, tool, &given, Verdict::Deny, why, None);
            return json!({ "ok": false, "error": format!("This machine paused: {why}.") });
        }
        let t0 = std::time::Instant::now();
        let out = self.dispatch(&sc, actor, tool, args, now).await;
        let ms = t0.elapsed().as_millis();
        match out {
            Ok((result, bytes)) => { self.note(&who, tool, &given, Verdict::Allow, &format!("ok ({ms} ms)"), Some(bytes)); json!({ "ok": true, "result": result }) }
            Err(Fail::Denied(why)) => { self.note(&who, tool, &given, Verdict::Deny, &why, None); json!({ "ok": false, "error": format!("Refused by this machine: {why}.") }) }
            Err(Fail::Error(e)) => { self.note(&who, tool, &given, Verdict::Allow, &format!("failed: {}", short(&e, 160)), None); json!({ "ok": false, "error": e }) }
        }
    }

    fn note(&self, who: &str, tool: &str, given: &str, d: Verdict, reason: &str, bytes: Option<u64>) {
        let mut e = Event::new(unix_now(), who, tool, given, d, reason);
        if let Some(b) = bytes { e = e.with_bytes(b); }
        let _ = self.audit.append(e);
    }

    async fn dispatch(&self, sc: &Scope, actor: &Actor, tool: &str, a: &Value, now: i64) -> Done {
        let max_file = self.loaded.read().unwrap_or_else(|e| e.into_inner()).max_file_mb << 20;
        match tool {
            "machine_info" => Ok((self.info(sc, now), 0)),
            "list" | "read" | "write" | "delete" | "mkdir" => {
                let (sc, a, tool) = (sc.clone(), a.clone(), tool.to_string());
                let Some(base) = sc.selected_folder(now).map(PathBuf::from) else { return Err(Fail::Denied("this scope has no selected folder".into())) };
                match tokio::task::spawn_blocking(move || file_tool(&sc, &base, &tool, &a, now, max_file)).await { Ok(r) => r, Err(_) => Err(Fail::Error("the file tool failed (a panic inside it); it was logged".into())) }
            }
            "shell_run" => self.shell_run(sc, a, now).await,
            "job_start" => self.job_start(sc, a, now),
            "job_read" => self.job_read(a),
            "job_stop" => self.job_stop(a).await,
            "proc_list" => self.proc_list(sc, a),
            "proc_kill" => self.proc_kill(sc, a),
            "re_list" | "re_run" | "re_logs" | "re_stop" => self.containers_call(tool, a, &actor.account).await,
            "transfer_get" | "transfer_put" => super::transfer::run(self, sc, tool, a, now, max_file).await,
            t => Err(Fail::Error(format!("Unknown tool {t}"))),
        }
    }

    /// `machine_info`: what the AI needs to know about its own limits (5.4: always in scope).
    fn info(&self, sc: &Scope, now: i64) -> Value {
        let tools: Vec<&str> = sc.tool_names(now).into_iter().filter(|t| IMPLEMENTED.contains(t)).collect();
        let sh = &sc.tools.shell;
        let cont = match lk(&self.containers).clone() { Some(c) => match c.available() { Ok(()) => json!({ "available": true }), Err(e) => json!({ "available": false, "why": e }) }, None => json!({ "available": false, "why": "no container engine on this machine" }) };
        json!({
            "machine": self.id, "app": self.app, "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "scope": sc.summary(now), "preset": sc.preset.as_str(), "selectedFolder": sc.selected_folder(now), "revision": sc.revision,
            "access": if sc.unrestricted { "unrestricted" } else if tools.contains(&"write") { "read-write" } else { "read-only" },
            "tools": tools,
            "shell": { "enabled": (sc.unrestricted || sh.enabled), "confinement": if sc.unrestricted { "none (unrestricted)" } else { "advisory: the working folder is confined to the scope, but a command can name any path its user can reach" }, "timeoutSec": sh.timeout_sec, "maxOutputKB": sh.max_output_kb, "elevation": "never" },
            "containers": cont,
            "jobs": { "running": self.jobs.running(), "max": self.loaded.read().unwrap_or_else(|e| e.into_inner()).max_jobs },
            "killSwitch": "a KILL file in the state folder, or `machine stop`, ends everything; only the owner clears it, on the machine",
        })
    }

    // ----------------------------------------------------------------------------------------------- shell

    fn shell_check(&self, sc: &Scope, a: &Value, now: i64) -> Result<(String, PathBuf), Fail> {
        let cmd = a["cmd"].as_str().or(a["command"].as_str()).unwrap_or("").to_string();
        if cmd.trim().is_empty() { return Err(Fail::Error("cmd is the command line to run.".into())); }
        if cmd.len() > 16_000 { return Err(Fail::Error("That command line is too long.".into())); }
        let d = sc.command_allowed(&cmd);
        if !d.allow { return Err(Fail::Denied(d.reason)); }
        if is_elevated() { return Err(Fail::Denied("this app runs with administrator / root rights, and the shell is never run that way".into())); }
        let base = sc.selected_folder(now).map(PathBuf::from).ok_or_else(|| Fail::Denied("this scope has no folder to run in".into()))?;
        let given = a["cwd"].as_str().unwrap_or("");
        // the cwd goes through the one canonicaliser and the scope (a read or a write entry both count)
        let r: Resolved = sc.authorize(Op::Shell, &base, given, now).map_err(|d| Fail::Denied(d.reason))?;
        let real = r.real.to_path_buf();
        if !real.is_dir() { return Err(Fail::Error(format!("cwd {} is not a folder.", if given.is_empty() { "(the selected folder)" } else { given }))); }
        Ok((cmd, real))
    }

    async fn shell_run(&self, sc: &Scope, a: &Value, now: i64) -> Done {
        let (cmd, cwd) = self.shell_check(sc, a, now)?;
        let cap = sc.tools.shell.timeout_sec.max(1);
        let secs = a["timeoutSec"].as_u64().filter(|n| *n > 0).map(|n| n.min(cap)).unwrap_or(cap);
        let out_cap = (sc.tools.shell.max_output_kb.max(1) as usize) << 10;
        let t0 = std::time::Instant::now();
        let mut child = shell_command(&cmd, &cwd).spawn().map_err(|e| Fail::Error(format!("could not start the command: {e}")))?;
        let pid = child.id().unwrap_or(0);
        self.jobs.children.add(pid);
        let rd = |s: Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>| tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let (mut buf, mut keep, mut cut) = (vec![0u8; 16 << 10], Vec::<u8>::new(), false);
            if let Some(mut s) = s { loop { match s.read(&mut buf).await { Ok(0) | Err(_) => break, Ok(n) => { if keep.len() < out_cap { let take = n.min(out_cap - keep.len()); keep.extend_from_slice(&buf[..take]); if take < n { cut = true; } } else { cut = true; } } } } }
            (keep, cut)
        });
        let o = rd(child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>));
        let e = rd(child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>));
        let (code, timed_out) = match tokio::time::timeout(Duration::from_secs(secs), child.wait()).await {
            Ok(Ok(s)) => (s.code(), false),
            Ok(Err(_)) => (None, false),
            Err(_) => { kill_tree(pid); let _ = child.wait().await; (None, true) }
        };
        kill_tree(pid); // what the command left running in its group
        self.jobs.children.remove(pid);
        let (so, co) = tokio::time::timeout(Duration::from_secs(2), o).await.ok().and_then(|r| r.ok()).unwrap_or_default();
        let (se, ce) = tokio::time::timeout(Duration::from_secs(2), e).await.ok().and_then(|r| r.ok()).unwrap_or_default();
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        let bytes = (so.len() + se.len()) as u64;
        Ok((json!({ "exit": code, "stdout": text(&so), "stderr": text(&se), "truncated": co || ce, "timedOut": timed_out, "cwd": cwd.display().to_string(), "ms": t0.elapsed().as_millis() as u64, "note": "advisory shell: its working folder is confined to this machine's scope, but a command can reach any path its user can" }), bytes))
    }

    fn job_start(&self, sc: &Scope, a: &Value, now: i64) -> Done {
        let (cmd, cwd) = self.shell_check(sc, a, now)?;
        let secs = a["timeoutSec"].as_u64().filter(|n| *n > 0).unwrap_or(DEFAULT_WALL_SECS);
        let max = self.loaded.read().unwrap_or_else(|e| e.into_inner()).max_jobs;
        let j = self.jobs.start(&cmd, &cwd, secs, max).map_err(Fail::Error)?;
        Ok((json!({ "jobId": j.id, "state": "running", "cwd": j.cwd, "wallSec": j.wall_secs, "hint": "job_read {jobId, since} returns new output; the job keeps running if the page reloads or the run ends (kept 30 minutes after it finishes); job_stop ends it." }), 0))
    }
    fn job_read(&self, a: &Value) -> Done {
        let Some(id) = a["jobId"].as_str().filter(|s| !s.is_empty()) else { return Ok((json!({ "jobs": self.jobs.list() }), 0)) };
        let Some(j) = self.jobs.get(id) else { return Err(Fail::Error(format!("No job {id} on this machine (it ended long ago, or the machine restarted).")) ) };
        let v = j.read(a["since"].as_u64().unwrap_or(0));
        let n = v["output"].as_str().map(|s| s.len() as u64).unwrap_or(0);
        Ok((v, n))
    }
    async fn job_stop(&self, a: &Value) -> Done {
        let id = a["jobId"].as_str().unwrap_or("");
        match self.jobs.stop(id).await { Some(j) => Ok((j.view(), 0)), None => Err(Fail::Error(format!("No job {id} on this machine."))) }
    }

    // ----------------------------------------------------------------------------------------------- processes

    fn proc_list(&self, sc: &Scope, _a: &Value) -> Done {
        if !sc.unrestricted && !sc.tools.proc_list { return Err(Fail::Denied("process listing is off in this scope".into())); }
        let own = self.jobs.children.all();
        let mut v: Vec<Value> = list_processes().into_iter().take(1000).map(|(pid, name)| json!({ "pid": pid, "name": name, "ownChild": own.contains(&pid) })).collect();
        v.sort_by_key(|x| x["pid"].as_u64().unwrap_or(0));
        Ok((json!({ "processes": v, "kill": match sc.tools.proc_kill { KillMode::None => "off", KillMode::OwnChildren => "own-children (only what a tool here started)", KillMode::Any => if sc.kill_any_allowed() { "any" } else { "own-children (any needs a whole-system scope)" } } }), 0))
    }
    fn proc_kill(&self, sc: &Scope, a: &Value) -> Done {
        let Some(pid) = a["pid"].as_u64().map(|p| p as u32).filter(|p| *p > 1) else { return Err(Fail::Error("pid is the process number.".into())) };
        if pid == std::process::id() { return Err(Fail::Denied("that is this app itself".into())); }
        let own = self.jobs.children.has(pid);
        if !own {
            let any = sc.kill_any_allowed();
            if sc.tools.proc_kill == KillMode::None && !sc.unrestricted { return Err(Fail::Denied("killing processes is off in this scope".into())); }
            if !any { return Err(Fail::Denied(format!("process {pid} was not started by a tool of this machine; this scope kills only its own children"))); }
        }
        if own { kill_tree(pid); } else { kill_one(pid); }
        Ok((json!({ "killed": pid, "ownChild": own }), 0))
    }

    // ----------------------------------------------------------------------------------------------- RE containers

    async fn containers_call(&self, tool: &str, a: &Value, who: &str) -> Done {
        let c = lk(&self.containers).clone();
        let Some(c) = c else { return Err(Fail::Error("There is no container engine on this machine (no Podman or Docker set up for the RE containers here), so re_* tools are not available.".into())) };
        let (tool, a, who) = (tool.to_string(), a.clone(), who.to_string());
        match tokio::task::spawn_blocking(move || { c.available().map_err(|e| format!("No container engine ready on this machine: {e}"))?; c.call(&tool, &a, &who) }).await {
            Ok(Ok(v)) => Ok((v, 0)),
            Ok(Err(e)) => Err(Fail::Error(e)),
            Err(_) => Err(Fail::Error("the container tool failed (a panic inside it); it was logged".into())),
        }
    }
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

/// Is this process running with administrator / root rights? (The shell is never run that way.)
fn is_elevated() -> bool {
    #[cfg(unix)]
    { let root = unsafe { libc::geteuid() } == 0; root && std::env::var("VR_MACHINE_ALLOW_ROOT").ok().as_deref() != Some("1") }
    #[cfg(not(unix))]
    { false }
}

fn kill_one(pid: u32) {
    #[cfg(unix)]
    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
    #[cfg(windows)]
    { let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status(); }
}

fn list_processes() -> Vec<(u32, String)> {
    #[cfg(target_os = "linux")]
    {
        let mut v = vec![];
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
                let name = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default().trim().to_string();
                v.push((pid, short(&name, 80)));
            }
        }
        v
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist").args(["/FO", "CSV", "/NH"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        out.lines().filter_map(|l| { let p: Vec<&str> = l.trim().trim_matches('"').split("\",\"").collect(); Some((p.get(1)?.parse::<u32>().ok()?, short(p.first()?, 80))) }).collect()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    { vec![] }
}

// --------------------------------------------------------------------------------------------------- file tools

pub(crate) fn authz(sc: &Scope, op: Op, base: &Path, given: &str, now: i64) -> Result<Resolved, Fail> { sc.authorize(op, base, given, now).map_err(|d| Fail::Denied(d.reason)) }

pub(crate) fn open_real(sc: &Scope, r: &Resolved, op: Op, mode: OpenMode, now: i64) -> Result<std::fs::File, Fail> {
    let verify = |p: &Path| match NormPath::from_native(p) { Ok(n) => sc.allow_norm(op, &n, None, now).allow, Err(_) => false };
    open_verified(r, &verify, mode).map_err(|e| match e { super::paths::PathError::Io(m) => Fail::Error(m), other => Fail::Denied(other.to_string()) })
}

/// The five tools of `pc_job`, through the scope. Same results as the AI host for the same folder.
pub(crate) fn file_tool(sc: &Scope, base: &Path, tool: &str, a: &Value, now: i64, max_file: u64) -> Done {
    use std::io::{Read, Seek, SeekFrom, Write};
    let given = a["path"].as_str().unwrap_or("");
    let rel = rel_of(given);
    match tool {
        "list" => {
            let r = authz(sc, Op::List, base, given, now)?;
            let mut v: Vec<String> = std::fs::read_dir(r.real.to_path_buf()).map_err(Fail::err)?.filter_map(|e| e.ok()).map(|e| { let n = e.file_name().to_string_lossy().to_string(); if e.path().is_dir() { n + "/" } else { n } }).collect();
            v.sort();
            let text = if v.is_empty() { "(empty folder)".to_string() } else { v.join("\n") };
            let n = text.len() as u64;
            Ok((json!(text), n))
        }
        "read" => {
            let r = authz(sc, Op::Read, base, given, now)?;
            let mut f = open_real(sc, &r, Op::Read, OpenMode::Read, now)?;
            let len = f.metadata().map_err(Fail::err)?.len();
            let (off, lim) = (a["offset"].as_u64(), a["limit"].as_u64());
            if off.is_none() && lim.is_none() {
                if len > READ_MAX { return Err(Fail::Error("That file is larger than 5 MB.".into())); }
                let mut s = String::new();
                f.read_to_string(&mut s).map_err(Fail::err)?;
                let n = s.len() as u64;
                return Ok((json!(s), n));
            }
            // a window of a long file: bytes from `offset`, at most `limit` (never more than 5 MB)
            let o = off.unwrap_or(0).min(len);
            let l = lim.unwrap_or(READ_MAX).min(READ_MAX).min(len - o);
            f.seek(SeekFrom::Start(o)).map_err(Fail::err)?;
            let mut buf = vec![0u8; l as usize];
            f.read_exact(&mut buf).map_err(Fail::err)?;
            let mut end = buf.len();
            if let Err(e) = std::str::from_utf8(&buf) { if e.error_len().is_none() { end = e.valid_up_to(); } }
            Ok((json!(String::from_utf8_lossy(&buf[..end]).into_owned()), end as u64))
        }
        "write" => {
            if rel.is_empty() { return Err(Fail::Error("Missing file name.".into())); }
            if given.ends_with('/') || given.ends_with('\\') { return Err(Fail::Error("That is a folder name, not a file.".into())); }
            let c = a["content"].as_str().unwrap_or("");
            if c.len() as u64 > max_file { return Err(Fail::Denied(format!("that is more than the scope's maxFileMB ({} MB)", max_file >> 20))); }
            let r = authz(sc, Op::Write, base, given, now)?;
            if let Some(p) = r.real.to_path_buf().parent() { std::fs::create_dir_all(p).map_err(Fail::err)?; }
            // atomic: a temp file beside it (checked like any write), then a rename onto the checked target
            let tmp_given = format!("{given}.vr-tmp-{}", pgm_core_internal::crypto::random_hex(4));
            let rt = authz(sc, Op::Write, base, &tmp_given, now)?;
            let mut f = open_real(sc, &rt, Op::Write, OpenMode::CreateNew, now)?;
            let put = f.write_all(c.as_bytes()).and_then(|_| f.flush());
            drop(f);
            let tmp_path = rt.real.to_path_buf();
            if let Err(e) = put { let _ = std::fs::remove_file(&tmp_path); return Err(Fail::err(e)); }
            let fin = authz(sc, Op::Write, base, given, now).and_then(|r2| std::fs::rename(&tmp_path, r2.real.to_path_buf()).map_err(Fail::err));
            if let Err(e) = fin { let _ = std::fs::remove_file(&tmp_path); return Err(e); }
            Ok((json!(format!("Wrote {rel} ({} chars)", c.chars().count())), c.len() as u64))
        }
        "delete" => {
            if rel.is_empty() { return Err(Fail::Error("Refusing to delete the whole folder.".into())); }
            let r = authz(sc, Op::Delete, base, given, now)?;
            let lex = r.lexical.to_path_buf();
            let gone = |e: std::io::Error| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(Fail::err(e)) };
            match std::fs::symlink_metadata(&lex) {
                Err(e) => gone(e)?,
                Ok(m) if m.file_type().is_symlink() => { std::fs::remove_file(&lex).or_else(|_| std::fs::remove_dir(&lex)).or_else(gone)?; }
                Ok(m) if m.is_dir() => {
                    let real = r.real.to_path_buf();
                    // deny wins inside a folder too: nothing protected may go with it
                    let mut stack = vec![real.clone()]; let mut seen = 0usize;
                    while let Some(d) = stack.pop() {
                        for e in std::fs::read_dir(&d).map_err(Fail::err)?.flatten() {
                            seen += 1; if seen > 200_000 { return Err(Fail::Denied("that folder holds too many entries to check before deleting".into())); }
                            let p = e.path();
                            let n = NormPath::from_native(&p).map_err(|e| Fail::Denied(e.to_string()))?;
                            let is_link = e.file_type().map(|t| t.is_symlink()).unwrap_or(true);
                            let d2 = sc.allow_norm(Op::Delete, &n, None, now);
                            if !d2.allow && !is_link { return Err(Fail::Denied(format!("the folder holds a protected entry ({})", d2.reason))); }
                            if !is_link && e.file_type().map(|t| t.is_dir()).unwrap_or(false) { stack.push(p); }
                        }
                    }
                    std::fs::remove_dir_all(&real).or_else(gone)?;
                }
                Ok(_) => { std::fs::remove_file(r.real.to_path_buf()).or_else(gone)?; }
            }
            Ok((json!(format!("Deleted {rel}")), 0))
        }
        "mkdir" => {
            if rel.is_empty() { authz(sc, Op::List, base, given, now)?; return Ok((json!("Created folder "), 0)); }
            let r = authz(sc, Op::Write, base, given, now)?;
            std::fs::create_dir_all(r.real.to_path_buf()).map_err(Fail::err)?;
            Ok((json!(format!("Created folder {rel}")), 0))
        }
        t => Err(Fail::Error(format!("Unknown tool {t}"))),
    }
}
