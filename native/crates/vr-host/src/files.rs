//! The host folder and the files in it: the same names the JavaScript host (bridge/ai-node.js) and its control app
//! (bridge/vr-host.js) use, so the Rust host can take the place of the JavaScript one in an existing folder, and either
//! control app works with either host.
//!   ai-node.json         the settings (holds the token: private)
//!   ai-node.status.json  the running host's live status, rewritten within a second of every change
//!   ai-node.lock         the running host's pid (one host per folder)
//!   ai-node.log          what it did (the service and the background start write it)
//!   ai-host.off          paused (the knife)
//!   .stop-now / .restart-now   "now" or "quiet": requests from the control app, picked up within 2 s
//!   .update-now          look for an update now
//!   .update-trial.json / .update-bad   an update on trial / one that was put back
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Folder { pub dir: PathBuf, pub config: PathBuf }
impl Folder {
    /// The folder: --config's folder, else VR_HOST_DIR, else the program's own folder.
    pub fn find(config: Option<&str>) -> Folder {
        if let Some(c) = config { let config = std::path::absolute(c).unwrap_or_else(|_| PathBuf::from(c)); let dir = config.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")); return Folder { dir, config }; }
        let dir = std::env::var_os("VR_HOST_DIR").map(PathBuf::from)
            .or_else(|| std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)))
            .unwrap_or_else(|| PathBuf::from("."));
        Folder { config: dir.join("ai-node.json"), dir }
    }
    pub fn f(&self, name: &str) -> PathBuf { self.dir.join(name) }
    pub fn status(&self) -> PathBuf { self.f("ai-node.status.json") }
    pub fn lock(&self) -> PathBuf { self.f("ai-node.lock") }
    pub fn log(&self) -> PathBuf { self.f("ai-node.log") }
    pub fn off(&self) -> PathBuf { self.f("ai-host.off") }
    pub fn stop_now(&self) -> PathBuf { self.f(".stop-now") }
    pub fn restart_now(&self) -> PathBuf { self.f(".restart-now") }
    pub fn update_now(&self) -> PathBuf { self.f(".update-now") }
    pub fn trial(&self) -> PathBuf { self.f(".update-trial.json") }
    pub fn bad(&self) -> PathBuf { self.f(".update-bad") }
    pub fn sandbox(&self) -> PathBuf { self.dir.join("..").join("Api").join("sandbox") }
    pub fn read_json(&self, p: &Path) -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) }
    pub fn status_json(&self) -> Value { self.read_json(&self.status()).unwrap_or_else(|| json!({})) }
    /// The pid in the lock file, if that process is alive.
    pub fn host_pid(&self) -> Option<u32> {
        let pid: u32 = std::fs::read_to_string(self.lock()).ok()?.trim().parse().ok()?;
        if pid != 0 && pid_alive(pid) { Some(pid) } else { None }
    }
}

pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    { let r = unsafe_kill0(pid); r }
    #[cfg(windows)]
    { crate::winsys::pid_alive(pid) }
}
#[cfg(unix)]
fn unsafe_kill0(pid: u32) -> bool {
    // kill(pid, 0): 0 = it exists; EPERM = it exists but belongs to someone else
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// One line in the log: "HH:MM:SS [ai-host] …" to stdout (the service / background start send stdout to ai-node.log).
pub fn log(msg: impl AsRef<str>) {
    let t = vr_core::now_ms() / 1000;
    let (h, m, s) = ((t / 3600) % 24, (t / 60) % 60, t % 60);
    println!("{h:02}:{m:02}:{s:02} [ai-host] {}", msg.as_ref());
}
