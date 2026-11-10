//! Long jobs on the machine (plans/linked-machines.md 6.2, T-311) and the processes the machine started.
//!
//! - `job_start` returns a job id at once. The job's output goes to a ring buffer (4 MB) and a file in the machine's work
//!   dir (`<state>/jobs/<id>.log`, a safety-floor path: no tool reads it as a file).
//! - The table lives on the MACHINE, not in a page or a run: a page reload, or a later run, reconnects by job id
//!   (`job_read` with no id lists them). A finished job is kept for [`JOB_TTL_SECS`] (30 minutes) so the person can come back.
//!   It does not survive the machine's restart, and it ends at revoke / the kill switch ([`JobTable::stop_all`]).
//! - Limits: concurrent jobs, a wall clock per job, the ring cap.
//! - [`Children`]: the pids a tool started. `proc_kill` with `kill: own-children` may only touch these.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;

/// A finished job stays this long (seconds).
pub const JOB_TTL_SECS: u64 = 30 * 60;
/// The ring buffer of one job.
pub const RING_BYTES: usize = 4 << 20;
/// The longest one `job_read` answers (bytes of output).
pub const READ_BYTES: usize = 256 << 10;
/// A job without a wall clock of its own may run this long (seconds).
pub const DEFAULT_WALL_SECS: u64 = 3600;
pub const MAX_WALL_SECS: u64 = 6 * 3600;

fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }
fn now_s() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

/// The pids the machine's tools started (shell commands and jobs): `proc_kill` with `own-children` touches only these.
#[derive(Default, Clone)]
pub struct Children(Arc<Mutex<HashSet<u32>>>);
impl Children {
    pub fn add(&self, pid: u32) { lk(&self.0).insert(pid); }
    pub fn remove(&self, pid: u32) { lk(&self.0).remove(&pid); }
    pub fn has(&self, pid: u32) -> bool { lk(&self.0).contains(&pid) }
    pub fn all(&self) -> Vec<u32> { lk(&self.0).iter().copied().collect() }
}

/// Ends a process the machine started and everything it started (its process group on Linux, `taskkill /T` on Windows).
pub fn kill_tree(pid: u32) {
    #[cfg(unix)]
    unsafe {
        if pid > 1 {
            libc::kill(-(pid as i32), libc::SIGKILL);
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = { #[allow(unused_mut)] let mut c = std::process::Command::new("taskkill"); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.args(["/PID", &pid.to_string(), "/T", "/F"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
}

/// The shell command for this system, ready to run in `cwd` (no stdin, own process group on Linux).
pub fn shell_command(cmd: &str, cwd: &Path) -> tokio::process::Command {
    #[cfg(unix)]
    let mut c = { let mut c = { #[allow(unused_mut)] let mut c = tokio::process::Command::new("sh"); #[cfg(windows)] { c.creation_flags(0x0800_0000); } /* CREATE_NO_WINDOW */ c }; c.arg("-c").arg(cmd); c.process_group(0); c };
    #[cfg(windows)]
    let mut c = { let mut c = { #[allow(unused_mut)] let mut c = tokio::process::Command::new("cmd"); #[cfg(windows)] { c.creation_flags(0x0800_0000); } /* CREATE_NO_WINDOW */ c }; c.arg("/C").arg(cmd); c.creation_flags(0x0800_0000 /* CREATE_NO_WINDOW */); c };
    c.current_dir(cwd).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    // what a tool's command must not inherit: the app's own settings
    for (k, _) in std::env::vars_os() {
        if let Some(k) = k.to_str() { if k.starts_with("VR_HOST_") || k.starts_with("MILLS_") || k == "VR_MACHINE_STATE" { c.env_remove(k); } }
    }
    c
}

/// Output kept in memory: the newest [`RING_BYTES`], with the offset of its first byte.
#[derive(Default)]
struct Ring { buf: Vec<u8>, base: u64 }
impl Ring {
    fn push(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
        if self.buf.len() > RING_BYTES { let cut = self.buf.len() - RING_BYTES; self.buf.drain(..cut); self.base += cut as u64; }
    }
    fn end(&self) -> u64 { self.base + self.buf.len() as u64 }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State { Running, Exited(i32), TimedOut, Stopped }

pub struct Job {
    pub id: String,
    pub cmd: String,
    pub cwd: String,
    pub started: u64,
    pub wall_secs: u64,
    pub pid: u32,
    state: Mutex<(State, u64)>, // state, finished at (seconds)
    ring: Mutex<Ring>,
    stop: AtomicBool,
    log: PathBuf,
}

impl Job {
    pub fn state(&self) -> State { lk(&self.state).0.clone() }
    fn finished_at(&self) -> u64 { lk(&self.state).1 }
    pub fn view(&self) -> Value {
        let (st, fin) = lk(&self.state).clone();
        let (label, exit) = match &st { State::Running => ("running", None), State::Exited(c) => ("exited", Some(*c)), State::TimedOut => ("timeout", None), State::Stopped => ("stopped", None) };
        json!({ "jobId": self.id, "cmd": self.cmd.chars().take(200).collect::<String>(), "cwd": self.cwd, "state": label, "running": st == State::Running, "exit": exit, "startedAt": self.started, "ageSec": now_s().saturating_sub(self.started), "finishedAt": if fin > 0 { json!(fin) } else { Value::Null }, "pid": self.pid })
    }
    /// New output since the byte offset `since` (output the ring has dropped is said so in `dropped`).
    pub fn read(&self, since: u64) -> Value {
        let ring = lk(&self.ring);
        let from = since.max(ring.base);
        let dropped = ring.base.saturating_sub(since);
        let start = (from - ring.base) as usize;
        let mut end = (start + READ_BYTES).min(ring.buf.len());
        // never cut a character in two
        if let Err(e) = std::str::from_utf8(&ring.buf[start..end]) { if e.error_len().is_none() { end = start + e.valid_up_to(); } }
        let text = String::from_utf8_lossy(&ring.buf[start..end]).into_owned();
        let mut v = self.view();
        v["output"] = json!(text);
        v["from"] = json!(from);
        v["next"] = json!(from + (end - start) as u64);
        v["more"] = json!(from + ((end - start) as u64) < ring.end());
        if dropped > 0 { v["dropped"] = json!(dropped); }
        v
    }
    fn set(&self, st: State) { let mut g = lk(&self.state); if g.0 == State::Running { *g = (st, now_s()); } }
}

#[derive(Default)]
pub struct JobTable {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    pub children: Children,
    dir: Mutex<Option<PathBuf>>,
}

impl JobTable {
    pub fn new(work_dir: &Path, children: Children) -> JobTable {
        let _ = std::fs::create_dir_all(work_dir);
        // jobs do not survive a restart: what an earlier process left is output nobody can read through the table
        if let Ok(rd) = std::fs::read_dir(work_dir) { for e in rd.flatten() { let _ = std::fs::remove_file(e.path()); } }
        JobTable { jobs: Mutex::default(), children, dir: Mutex::new(Some(work_dir.to_path_buf())) }
    }
    pub fn running(&self) -> usize { lk(&self.jobs).values().filter(|j| j.state() == State::Running).count() }
    pub fn get(&self, id: &str) -> Option<Arc<Job>> { lk(&self.jobs).get(id).cloned() }
    pub fn list(&self) -> Vec<Value> {
        let mut v: Vec<Arc<Job>> = lk(&self.jobs).values().cloned().collect();
        v.sort_by_key(|j| j.started);
        v.iter().map(|j| j.view()).collect()
    }
    /// Starts `cmd` in `cwd`. Err = why not (too many jobs, the shell would not start).
    pub fn start(self: &Arc<Self>, cmd: &str, cwd: &Path, wall_secs: u64, max_running: usize) -> Result<Arc<Job>, String> {
        if self.running() >= max_running { return Err(format!("{max_running} jobs are running already (the scope's concurrentJobs); stop one or wait for it.")); }
        let id = pgm_core_internal::crypto::random_hex(6);
        let mut child = shell_command(cmd, cwd).spawn().map_err(|e| format!("could not start the command: {e}"))?;
        let pid = child.id().unwrap_or(0);
        self.children.add(pid);
        let log = lk(&self.dir).clone().map(|d| d.join(format!("{id}.log"))).unwrap_or_default();
        let job = Arc::new(Job { id: id.clone(), cmd: cmd.to_string(), cwd: cwd.display().to_string(), started: now_s(), wall_secs: wall_secs.clamp(1, MAX_WALL_SECS), pid, state: Mutex::new((State::Running, 0)), ring: Mutex::default(), stop: AtomicBool::new(false), log: log.clone() });
        lk(&self.jobs).insert(id, job.clone());
        let file = if log.as_os_str().is_empty() { None } else { std::fs::OpenOptions::new().create(true).append(true).open(&log).ok().map(|f| Arc::new(Mutex::new(f))) };
        let mut readers = vec![];
        let mut streams: Vec<Box<dyn tokio::io::AsyncRead + Send + Unpin>> = vec![];
        if let Some(s) = child.stdout.take() { streams.push(Box::new(s)); }
        if let Some(s) = child.stderr.take() { streams.push(Box::new(s)); }
        for stream in streams {
            let (j, f) = (job.clone(), file.clone());
            readers.push(tokio::spawn(async move {
                let mut s = stream; let mut buf = vec![0u8; 16 << 10];
                loop {
                    match s.read(&mut buf).await { Ok(0) | Err(_) => break, Ok(n) => { lk(&j.ring).push(&buf[..n]); if let Some(f) = &f { let _ = lk(f).write_all(&buf[..n]); } } }
                }
            }));
        }
        let (j, children, wall) = (job.clone(), self.children.clone(), job.wall_secs);
        tokio::spawn(async move {
            let res = tokio::time::timeout(std::time::Duration::from_secs(wall), child.wait()).await;
            let st = match res {
                Ok(Ok(s)) => if j.stop.load(Ordering::SeqCst) { State::Stopped } else { State::Exited(s.code().unwrap_or(-1)) },
                Ok(Err(_)) => State::Exited(-1),
                Err(_) => { kill_tree(pid); let _ = child.wait().await; State::TimedOut }
            };
            // anything the command left running in its group goes with it
            kill_tree(pid);
            for r in readers { let _ = tokio::time::timeout(std::time::Duration::from_millis(700), r).await; }
            children.remove(pid);
            j.set(st);
        });
        Ok(job)
    }
    /// Stops one job (its whole process tree); waits briefly for the state to change.
    pub async fn stop(&self, id: &str) -> Option<Arc<Job>> {
        let j = self.get(id)?;
        if j.state() == State::Running { j.stop.store(true, Ordering::SeqCst); kill_tree(j.pid); for _ in 0..40 { if j.state() != State::Running { break; } tokio::time::sleep(std::time::Duration::from_millis(50)).await; } }
        Some(j)
    }
    /// The revoke / kill switch: every job ends, the table is emptied, the output files go.
    pub fn stop_all(&self) -> usize {
        let all: Vec<Arc<Job>> = lk(&self.jobs).drain().map(|(_, j)| j).collect();
        let mut n = 0;
        for j in &all { if j.state() == State::Running { j.stop.store(true, Ordering::SeqCst); kill_tree(j.pid); n += 1; } let _ = std::fs::remove_file(&j.log); }
        for pid in self.children.all() { kill_tree(pid); }
        n
    }
    /// Drops finished jobs older than the keep time (and their files). Returns how many.
    pub fn reap(&self, ttl_secs: u64) -> usize {
        let now = now_s();
        let mut jobs = lk(&self.jobs);
        let old: Vec<String> = jobs.iter().filter(|(_, j)| { let f = j.finished_at(); f > 0 && now.saturating_sub(f) > ttl_secs }).map(|(k, _)| k.clone()).collect();
        for k in &old { if let Some(j) = jobs.remove(k) { let _ = std::fs::remove_file(&j.log); } }
        old.len()
    }
}
