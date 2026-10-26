//! The kill switch (spec 7.2, R332): the machine's own refusal.
//!
//! - **Stopped** is a file: `<state>/KILL`. Creating it by hand (a filesystem tool when the UI hangs) or calling
//!   [`Kill::stop`] (`vr-host machine stop`, the tray item, the hot key) puts the machine in the stopped state: no
//!   jobs ([`Kill::admit`] refuses), no link, and the poll hooks ([`StopHooks`]) fire once within the watch interval
//!   (2 s by default). The state is the file, so it **survives a restart** with nothing else to remember.
//! - Only a **local** action clears it: [`Kill::clear`] needs a [`LocalApproval`], which only the machine's own UI or
//!   terminal code builds. A message from the hub goes through [`Kill::remote_message`]: a remote `stop` is welcome
//!   (stopping is always safe), every other word (start, clear, resume ...) is refused, and nothing a remote
//!   message says removes the file.
//! - The file is a safety-floor path ([`super::changes::state_floor`]): no AI tool can delete it, so no tool can
//!   restart the machine. (An AI creating it only stops itself.)
//! - A `KILL` that cannot be inspected (the folder is unreadable) counts as stopped: fail closed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const KILL_FILE: &str = "KILL";
/// The service looks every 2 s (7.2).
pub const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// Proof that the person is at the machine: built only by the machine's own screen or terminal command, never by
/// the hub routes or a tool handler. Kill-clear, elevation approval and scope approval all need one.
#[derive(Clone, Debug)]
pub struct LocalApproval {
    by: String,
}

impl LocalApproval {
    /// `by` names the surface for the audit line: `machine-ui`, `terminal`, `tray`.
    pub fn from_machine(by: &str) -> LocalApproval {
        LocalApproval { by: by.to_string() }
    }
    pub fn by(&self) -> &str {
        &self.by
    }
}

/// What the stopped machine says when something asks it to work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stopped(pub String);

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stopped on the machine: {}", self.0)
    }
}
impl std::error::Error for Stopped {}

/// What to do when the machine goes to stopped: end every job, close the link and the poll.
pub trait StopHooks: Send + Sync {
    fn stop_jobs(&self);
    fn drop_link(&self);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteReply {
    /// A remote `stop` was applied.
    Stopped,
    /// Refused, with the words for the hub.
    Refused(String),
}

pub struct Kill {
    file: PathBuf,
    fired: AtomicBool,
}

impl Kill {
    pub fn open(state_dir: impl AsRef<Path>) -> Kill {
        Kill { file: state_dir.as_ref().join(KILL_FILE), fired: AtomicBool::new(false) }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn is_stopped(&self) -> bool {
        match std::fs::symlink_metadata(&self.file) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        }
    }

    /// Why / when / by whom, when the file says (a hand-made empty `KILL` has no details).
    pub fn info(&self) -> Option<String> {
        if !self.is_stopped() {
            return None;
        }
        Some(std::fs::read_to_string(&self.file).unwrap_or_default().trim().to_string())
    }

    /// `machine stop`: write the file. Idempotent.
    pub fn stop(&self, by: &str, reason: &str, now: i64) -> std::io::Result<()> {
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d)?;
        }
        if self.is_stopped() {
            return Ok(());
        }
        std::fs::write(&self.file, serde_json::json!({"t": now, "by": by, "reason": reason}).to_string())
    }

    /// The guard every job start, tool call and link attempt passes first.
    pub fn admit(&self) -> Result<(), Stopped> {
        if self.is_stopped() {
            Err(Stopped(self.info().filter(|s| !s.is_empty()).unwrap_or_else(|| "KILL file".into())))
        } else {
            Ok(())
        }
    }

    /// Start access again: local only. Returns whether it had been stopped.
    pub fn clear(&self, _approval: &LocalApproval) -> std::io::Result<bool> {
        let was = self.is_stopped();
        match std::fs::remove_file(&self.file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.fired.store(false, Ordering::SeqCst);
        Ok(was)
    }

    /// A message from the hub about the kill state. Only `stop` does anything.
    pub fn remote_message(&self, msg: &str, now: i64) -> RemoteReply {
        if msg.trim().eq_ignore_ascii_case("stop") {
            return match self.stop("hub", "stopped from the web", now) {
                Ok(()) => RemoteReply::Stopped,
                Err(e) => RemoteReply::Refused(format!("could not stop: {e}")),
            };
        }
        RemoteReply::Refused("the web cannot restart a stopped machine; start access again on the machine".into())
    }

    /// Look once: when stopped and the hooks have not run for this stop, run them. Returns whether stopped.
    pub fn poll(&self, hooks: &dyn StopHooks) -> bool {
        if self.is_stopped() {
            if !self.fired.swap(true, Ordering::SeqCst) {
                hooks.stop_jobs();
                hooks.drop_link();
            }
            true
        } else {
            self.fired.store(false, Ordering::SeqCst);
            false
        }
    }

    /// Poll in a thread every `interval` until the handle is dropped or stopped.
    pub fn watch(self: Arc<Kill>, hooks: Arc<dyn StopHooks>, interval: Duration) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let join = std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                self.poll(hooks.as_ref());
                std::thread::sleep(interval);
            }
        });
        Watcher { stop, join: Some(join) }
    }
}

pub struct Watcher {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub fn end(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pgm-kill-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[derive(Default)]
    struct Hooks {
        jobs: AtomicUsize,
        link: AtomicUsize,
    }
    impl StopHooks for Hooks {
        fn stop_jobs(&self) {
            self.jobs.fetch_add(1, Ordering::SeqCst);
        }
        fn drop_link(&self) {
            self.link.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn r332_stop_call_stops_jobs_and_link_and_survives_a_restart() {
        let d = tmp("stop");
        let k = Kill::open(&d);
        let h = Hooks::default();
        assert!(k.admit().is_ok());
        assert!(!k.poll(&h));
        k.stop("terminal", "machine stop", 100).unwrap();
        assert!(k.file().exists());
        assert!(k.admit().is_err(), "no jobs, no link");
        assert!(k.poll(&h));
        assert!(k.poll(&h));
        assert_eq!((h.jobs.load(Ordering::SeqCst), h.link.load(Ordering::SeqCst)), (1, 1), "hooks run once per stop");
        // a restart: a new Kill over the same folder is still stopped, and its hooks run again
        let k2 = Kill::open(&d);
        assert!(k2.is_stopped());
        assert!(k2.admit().unwrap_err().to_string().contains("machine stop"));
        let h2 = Hooks::default();
        assert!(k2.poll(&h2));
        assert_eq!((h2.jobs.load(Ordering::SeqCst), h2.link.load(Ordering::SeqCst)), (1, 1));
    }

    #[test]
    fn r332_a_hand_made_kill_file_stops_it_within_the_watch_interval() {
        let d = tmp("file");
        let k = Arc::new(Kill::open(&d));
        let h = Arc::new(Hooks::default());
        let w = k.clone().watch(h.clone(), Duration::from_millis(20));
        std::fs::write(d.join("KILL"), "").unwrap();
        for _ in 0..100 {
            if h.link.load(Ordering::SeqCst) == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        w.end();
        assert_eq!((h.jobs.load(Ordering::SeqCst), h.link.load(Ordering::SeqCst)), (1, 1));
        assert!(k.admit().is_err());
        assert!(WATCH_INTERVAL <= Duration::from_secs(2));
    }

    #[test]
    fn r332_only_a_local_action_clears_it_the_web_cannot() {
        let d = tmp("clear");
        let k = Kill::open(&d);
        k.stop("tray", "stop all access now", 1).unwrap();
        for msg in ["start", "clear", "resume", "unstop", "kill:off", "delete KILL", "", "stop; start", "START"] {
            assert!(matches!(k.remote_message(msg, 2), RemoteReply::Refused(_)), "{msg:?}");
            assert!(k.is_stopped(), "{msg:?} cleared it");
        }
        assert_eq!(k.remote_message("stop", 3), RemoteReply::Stopped);
        assert!(k.is_stopped());
        assert!(k.clear(&LocalApproval::from_machine("terminal")).unwrap());
        assert!(!k.is_stopped());
        assert!(k.admit().is_ok());
        assert!(!k.clear(&LocalApproval::from_machine("terminal")).unwrap(), "already running");
        // a remote stop works on a running machine, and it also needs a local start
        assert_eq!(k.remote_message("Stop", 4), RemoteReply::Stopped);
        assert!(k.is_stopped());
    }

    #[test]
    #[cfg(unix)]
    fn r332_an_unreadable_state_counts_as_stopped() {
        let d = tmp("fail");
        // the "state dir" is a file, so KILL cannot be looked at: not NotFound but NotADirectory
        let f = d.join("file");
        std::fs::write(&f, "x").unwrap();
        let k = Kill::open(&f);
        assert!(k.is_stopped());
    }
}
