//! Elevation (spec 7.5, R332).
//!
//! - **The app never runs elevated.** [`refuse_if_elevated`] looks at the process (root on Linux, a high-integrity
//!   token on Windows) and refuses to serve. Experts can start with [`ALLOW_ELEVATED_FLAG`]; the refusal text
//!   names it. Without the flag the machine does not start, so a tool never has more than the person's account.
//! - **`elevate {reason, command}` is never silent.** [`elevate`] needs `tools.elevate: true` in the scope, then
//!   puts the exact command in the pending-approval queue (`<state>/elevate-pending.json`, a safety-floor path)
//!   and asks the **local prompt hook** ([`Prompt`]: the tray / terminal UI later; [`QueuePrompt`] waits for the
//!   tray to approve through the queue file). Approval is per request, never remembered, and expires after 60 s.
//!   Anything but an approval in time is `Refused`, and the command is **never run**. An approved request runs
//!   **once** (through [`ElevatedRunner`]: `pkexec` / polkit or UAC `runas` later) and cannot be run again.
//! - Every step leaves an audit line. Nothing here takes a command from the web without the prompt.

use super::audit::{AuditLog, Event, Verdict};
use super::kill::{Kill, LocalApproval};
use super::scope::Scope;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const QUEUE_FILE: &str = "elevate-pending.json";
/// Denied or unanswered in 60 s is "refused" (7.5).
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);
/// The expert override of the "never run elevated" rule.
pub const ALLOW_ELEVATED_FLAG: &str = "--i-know-this-is-dangerous-run-elevated";

// ------------------------------------------------------------------ the app itself

/// Is this process root / Administrator (high integrity)?
pub fn is_elevated() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no arguments and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(windows)]
    {
        // The high mandatory level SID in the token's groups: present only in an elevated process.
        std::process::Command::new("whoami").arg("/groups").output().map(|o| String::from_utf8_lossy(&o.stdout).contains("S-1-16-12288")).unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// The pure rule (testable without being root): serve only when not elevated, or when the expert flag is given.
pub fn check_elevated(elevated: bool, allow_flag: bool) -> Result<(), String> {
    if elevated && !allow_flag {
        Err(format!("refusing to serve as root / Administrator: the machine runs as your own account so a tool can never be more than you. Start it from a normal account. (Experts only: {ALLOW_ELEVATED_FLAG})"))
    } else {
        Ok(())
    }
}

/// `args` are the command-line arguments; call this before the machine serves or links.
pub fn refuse_if_elevated(args: &[String]) -> Result<(), String> {
    check_elevated(is_elevated(), args.iter().any(|a| a == ALLOW_ELEVATED_FLAG))
}

// ------------------------------------------------------------------ the queue

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Approved,
    Denied,
    Expired,
    Running,
    Done,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Approved => "approved",
            Status::Denied => "denied",
            Status::Expired => "expired",
            Status::Running => "running",
            Status::Done => "done",
        }
    }
    fn parse(s: &str) -> Status {
        match s {
            "approved" => Status::Approved,
            "denied" => Status::Denied,
            "expired" => Status::Expired,
            "running" => Status::Running,
            "done" => Status::Done,
            // anything unknown is the safest: not approved
            "pending" => Status::Pending,
            _ => Status::Denied,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElevateRequest {
    /// The account that asked (named in the prompt).
    pub account: String,
    pub run: String,
    pub reason: String,
    /// Shown exactly as it will run.
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: String,
    pub t: i64,
    pub req: ElevateRequest,
    pub status: Status,
    pub decided_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ElevateError {
    /// `tools.elevate` is off, or the scope is unreadable.
    NotEnabled,
    Stopped(String),
    /// The scope's command rules refuse it.
    CommandDenied(String),
    /// Denied, unanswered in 60 s, or no prompt: the command did not run.
    Refused(String),
    /// Approved but the runner failed.
    Failed(String),
    /// The request is not in the state this step needs (unknown id, already run, not approved).
    BadState(String),
    Io(String),
}

impl std::fmt::Display for ElevateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ElevateError::NotEnabled => write!(f, "needs elevation; elevation is not enabled in this machine's scope"),
            ElevateError::Stopped(s) => write!(f, "stopped on the machine: {s}"),
            ElevateError::CommandDenied(s) => write!(f, "command not allowed: {s}"),
            ElevateError::Refused(s) => write!(f, "refused: {s}"),
            ElevateError::Failed(s) => write!(f, "elevated command failed: {s}"),
            ElevateError::BadState(s) => write!(f, "{s}"),
            ElevateError::Io(s) => write!(f, "queue: {s}"),
        }
    }
}
impl std::error::Error for ElevateError {}

pub struct ElevateQueue {
    file: PathBuf,
}

impl ElevateQueue {
    pub fn open(state_dir: impl AsRef<Path>) -> ElevateQueue {
        ElevateQueue { file: state_dir.as_ref().join(QUEUE_FILE) }
    }
    pub fn file(&self) -> &Path {
        &self.file
    }

    fn load(&self) -> Vec<Pending> {
        let Ok(text) = std::fs::read_to_string(&self.file) else { return vec![] };
        let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&text) else { return vec![] };
        a.iter()
            .filter_map(|v| {
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
                Some(Pending {
                    id: s("id")?,
                    t: v.get("t")?.as_i64()?,
                    req: ElevateRequest { account: s("account")?, run: s("run").unwrap_or_default(), reason: s("reason").unwrap_or_default(), command: s("command")? },
                    status: Status::parse(&s("status")?),
                    decided_by: s("decidedBy"),
                })
            })
            .collect()
    }

    fn save(&self, items: &[Pending]) -> Result<(), ElevateError> {
        let v: Vec<Value> = items
            .iter()
            .map(|p| json!({"id": p.id, "t": p.t, "account": p.req.account, "run": p.req.run, "reason": p.req.reason, "command": p.req.command, "status": p.status.as_str(), "decidedBy": p.decided_by}))
            .collect();
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d).map_err(|e| ElevateError::Io(e.to_string()))?;
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, Value::Array(v).to_string()).and_then(|_| std::fs::rename(&tmp, &self.file)).map_err(|e| ElevateError::Io(e.to_string()))
    }

    pub fn list(&self) -> Vec<Pending> {
        self.load()
    }
    pub fn get(&self, id: &str) -> Option<Pending> {
        self.load().into_iter().find(|p| p.id == id)
    }

    /// Queue a request (status `pending`). Nothing runs.
    pub fn request(&self, req: ElevateRequest, now: i64) -> Result<String, ElevateError> {
        let mut items = self.load();
        // keep the file small: the settled ones older than the newest 100 go
        let n = items.len();
        if n > 100 {
            items.drain(..n - 100);
        }
        let id = format!("e{}-{}", now, items.len() + 1);
        items.push(Pending { id: id.clone(), t: now, req, status: Status::Pending, decided_by: None });
        self.save(&items)?;
        Ok(id)
    }

    fn set(&self, id: &str, from: &[Status], to: Status, by: Option<&str>) -> Result<(), ElevateError> {
        let mut items = self.load();
        let p = items.iter_mut().find(|p| p.id == id).ok_or_else(|| ElevateError::BadState(format!("no elevation request {id}")))?;
        if !from.contains(&p.status) {
            return Err(ElevateError::BadState(format!("request {id} is {}, not {}", p.status.as_str(), from.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" or "))));
        }
        p.status = to;
        if let Some(b) = by {
            p.decided_by = Some(b.to_string());
        }
        self.save(&items)
    }

    /// The person says yes, on the machine. Only a `pending` request can be approved (not an expired one).
    pub fn approve(&self, id: &str, by: &LocalApproval) -> Result<(), ElevateError> {
        self.set(id, &[Status::Pending], Status::Approved, Some(by.by()))
    }
    pub fn deny(&self, id: &str, by: &LocalApproval) -> Result<(), ElevateError> {
        self.set(id, &[Status::Pending], Status::Denied, Some(by.by()))
    }
    fn expire(&self, id: &str) {
        let _ = self.set(id, &[Status::Pending], Status::Expired, None);
    }
    /// approved -> running, exactly once: a second caller gets BadState.
    fn begin_run(&self, id: &str) -> Result<(), ElevateError> {
        self.set(id, &[Status::Approved], Status::Running, None)
    }
    fn finish(&self, id: &str) {
        let _ = self.set(id, &[Status::Running], Status::Done, None);
    }
}

// ------------------------------------------------------------------ the prompt and the runner

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Approve,
    Deny,
    NoAnswer,
}

/// The local prompt hook. The tray / terminal UI implements it: it must show the exact command and the account
/// that asked, and return only when **the person at the machine** answered or `timeout` passed.
pub trait Prompt {
    fn ask(&self, pending: &Pending, timeout: Duration) -> Answer;
}

/// No UI wired: nobody can approve, so elevation fails closed.
pub struct NoPrompt;
impl Prompt for NoPrompt {
    fn ask(&self, _p: &Pending, _t: Duration) -> Answer {
        Answer::NoAnswer
    }
}

/// Waits for the tray UI to approve or deny through the queue file (`ElevateQueue::approve / deny`).
pub struct QueuePrompt<'a> {
    pub queue: &'a ElevateQueue,
    pub poll: Duration,
}
impl Prompt for QueuePrompt<'_> {
    fn ask(&self, p: &Pending, timeout: Duration) -> Answer {
        let start = Instant::now();
        loop {
            match self.queue.get(&p.id).map(|x| x.status) {
                Some(Status::Approved) => return Answer::Approve,
                Some(Status::Denied) => return Answer::Deny,
                Some(Status::Pending) => {}
                _ => return Answer::NoAnswer,
            }
            if start.elapsed() >= timeout {
                return Answer::NoAnswer;
            }
            std::thread::sleep(self.poll);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElevatedResult {
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs one approved command with the OS's elevation (`pkexec` on Linux, `runas` / UAC on Windows; later).
pub trait ElevatedRunner {
    fn run_once(&self, command: &str) -> Result<ElevatedResult, String>;
}

// ------------------------------------------------------------------ the call

pub struct Elevator<'a> {
    pub scope: &'a Scope,
    pub kill: &'a Kill,
    pub queue: &'a ElevateQueue,
    pub prompt: &'a dyn Prompt,
    pub runner: &'a dyn ElevatedRunner,
    pub audit: Option<&'a AuditLog>,
    pub timeout: Duration,
}

impl Elevator<'_> {
    fn log(&self, now: i64, req: &ElevateRequest, v: Verdict, why: &str) {
        if let Some(a) = self.audit {
            let _ = a.append(Event::new(now, &req.account, "elevate", &req.command, v, why));
        }
    }

    /// The AI's `elevate {reason, command}`. Returns the result only after a local approval; every other path is an
    /// error and the command has **not** run.
    pub fn elevate(&self, req: ElevateRequest, now: i64) -> Result<ElevatedResult, ElevateError> {
        if let Err(s) = self.kill.admit() {
            self.log(now, &req, Verdict::Deny, &s.to_string());
            return Err(ElevateError::Stopped(s.0));
        }
        if self.scope.failed() || !self.scope.tools.elevate {
            self.log(now, &req, Verdict::Deny, "elevation is not enabled in the scope");
            return Err(ElevateError::NotEnabled);
        }
        if req.command.trim().is_empty() || req.command.contains('\0') {
            self.log(now, &req, Verdict::Deny, "empty or invalid command");
            return Err(ElevateError::CommandDenied("empty or invalid command".into()));
        }
        let d = self.scope.command_allowed(&req.command);
        if !d.allow {
            self.log(now, &req, Verdict::Deny, &d.reason);
            return Err(ElevateError::CommandDenied(d.reason));
        }
        let id = self.queue.request(req.clone(), now)?;
        self.log(now, &req, Verdict::Prompt, &format!("asked the person on the machine ({id})"));
        let pending = self.queue.get(&id).ok_or_else(|| ElevateError::Io("request vanished".into()))?;
        let started = Instant::now();
        let answer = self.prompt.ask(&pending, self.timeout);
        let in_time = started.elapsed() <= self.timeout + Duration::from_millis(500);
        match (answer, in_time) {
            (Answer::Approve, true) => {
                // the queue is the record: an approval that did not go through it (a hook that just says yes) is
                // recorded as the prompt's, and the single run is still enforced by the state change
                let st = self.queue.get(&id).map(|p| p.status);
                if st == Some(Status::Pending) {
                    self.queue.approve(&id, &LocalApproval::from_machine("prompt"))?;
                }
                self.queue.begin_run(&id)?;
                self.log(now, &req, Verdict::Allow, &format!("approved on the machine ({id}); running once"));
                let r = self.runner.run_once(&req.command);
                self.queue.finish(&id);
                match r {
                    Ok(res) => Ok(res),
                    Err(e) => {
                        self.log(now, &req, Verdict::Deny, &format!("runner failed: {e}"));
                        Err(ElevateError::Failed(e))
                    }
                }
            }
            (Answer::Deny, _) => {
                let _ = self.queue.deny(&id, &LocalApproval::from_machine("prompt"));
                self.log(now, &req, Verdict::Deny, &format!("denied on the machine ({id})"));
                Err(ElevateError::Refused("the person at the machine said no".into()))
            }
            _ => {
                self.queue.expire(&id);
                self.log(now, &req, Verdict::Deny, &format!("no approval in {} s ({id})", self.timeout.as_secs()));
                Err(ElevateError::Refused(format!("not approved on the machine in {} s", self.timeout.as_secs())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pgm-elev-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[derive(Default)]
    struct Runner {
        ran: RefCell<Vec<String>>,
        fail: Cell<bool>,
    }
    impl ElevatedRunner for Runner {
        fn run_once(&self, c: &str) -> Result<ElevatedResult, String> {
            self.ran.borrow_mut().push(c.to_string());
            if self.fail.get() {
                Err("pkexec dismissed".into())
            } else {
                Ok(ElevatedResult { exit: 0, stdout: "ok".into(), stderr: String::new() })
            }
        }
    }
    struct Says(Answer);
    impl Prompt for Says {
        fn ask(&self, _p: &Pending, _t: Duration) -> Answer {
            self.0
        }
    }

    fn scope(elevate: bool) -> Scope {
        let tmp = serde_json::to_string(std::env::temp_dir().to_str().unwrap()).unwrap();
        let j = format!(r#"{{"preset":"custom","paths":[{{"path":{tmp},"access":"rw"}}],"tools":{{"elevate":{elevate},"shell":{{"denyCommands":["mkfs"]}}}}}}"#);
        Scope::try_parse(&j).unwrap()
    }
    fn req(c: &str) -> ElevateRequest {
        ElevateRequest { account: "sponge".into(), run: "r1".into(), reason: "install".into(), command: c.into() }
    }

    #[test]
    fn r332_elevate_without_the_machines_approval_fails_and_never_runs() {
        let d = tmp("noapprove");
        let (k, q, r) = (Kill::open(&d), ElevateQueue::open(&d), Runner::default());
        let log = AuditLog::open(d.join("audit.jsonl")).unwrap();
        let s = scope(true);
        for (prompt, what) in [(&NoPrompt as &dyn Prompt, "no prompt wired"), (&Says(Answer::Deny), "denied"), (&Says(Answer::NoAnswer), "unanswered")] {
            let e = Elevator { scope: &s, kill: &k, queue: &q, prompt, runner: &r, audit: Some(&log), timeout: Duration::from_secs(60) };
            let err = e.elevate(req("apt install foo"), 10).unwrap_err();
            assert!(matches!(err, ElevateError::Refused(_)), "{what}: {err:?}");
        }
        assert!(r.ran.borrow().is_empty(), "the command never ran");
        assert!(q.list().iter().all(|p| p.status != Status::Approved && p.status != Status::Done));
        assert!(log.records().iter().any(|x| x.op == "elevate" && x.decision == "prompt"));
        assert!(log.verify().ok);
    }

    #[test]
    fn r332_elevate_needs_the_scope_flag_a_running_machine_and_an_allowed_command() {
        let d = tmp("gates");
        let (k, q, r) = (Kill::open(&d), ElevateQueue::open(&d), Runner::default());
        let yes = Says(Answer::Approve);
        let off = scope(false);
        let e = Elevator { scope: &off, kill: &k, queue: &q, prompt: &yes, runner: &r, audit: None, timeout: ANSWER_TIMEOUT };
        assert_eq!(e.elevate(req("apt install foo"), 1).unwrap_err(), ElevateError::NotEnabled);
        let bad = Scope::nothing();
        let e = Elevator { scope: &bad, kill: &k, queue: &q, prompt: &yes, runner: &r, audit: None, timeout: ANSWER_TIMEOUT };
        assert_eq!(e.elevate(req("x"), 1).unwrap_err(), ElevateError::NotEnabled);
        let on = scope(true);
        let e = Elevator { scope: &on, kill: &k, queue: &q, prompt: &yes, runner: &r, audit: None, timeout: ANSWER_TIMEOUT };
        assert!(matches!(e.elevate(req("mkfs /dev/sda"), 1).unwrap_err(), ElevateError::CommandDenied(_)));
        assert!(matches!(e.elevate(req("   "), 1).unwrap_err(), ElevateError::CommandDenied(_)));
        k.stop("terminal", "t", 1).unwrap();
        assert!(matches!(e.elevate(req("apt install foo"), 1).unwrap_err(), ElevateError::Stopped(_)));
        assert!(r.ran.borrow().is_empty());
        assert!(q.list().is_empty(), "none of these even queued");
    }

    #[test]
    fn r332_an_approved_request_runs_once_and_is_audited() {
        let d = tmp("approve");
        let (k, q, r) = (Kill::open(&d), ElevateQueue::open(&d), Runner::default());
        let log = AuditLog::open(d.join("audit.jsonl")).unwrap();
        let s = scope(true);
        let yes = Says(Answer::Approve);
        let e = Elevator { scope: &s, kill: &k, queue: &q, prompt: &yes, runner: &r, audit: Some(&log), timeout: ANSWER_TIMEOUT };
        let res = e.elevate(req("apt install foo"), 5).unwrap();
        assert_eq!(res.exit, 0);
        assert_eq!(r.ran.borrow().as_slice(), ["apt install foo"]);
        let item = &q.list()[0];
        assert_eq!(item.status, Status::Done);
        // no "remember": the next call asks again
        let e2 = Elevator { scope: &s, kill: &k, queue: &q, prompt: &NoPrompt, runner: &r, audit: Some(&log), timeout: ANSWER_TIMEOUT };
        assert!(e2.elevate(req("apt install foo"), 6).is_err());
        assert_eq!(r.ran.borrow().len(), 1);
        // a done request cannot be approved or run again
        let me = LocalApproval::from_machine("tray");
        assert!(q.approve(&item.id, &me).is_err());
        assert!(q.begin_run(&item.id).is_err());
        // a failing runner is reported, not hidden
        r.fail.set(true);
        assert!(matches!(e.elevate(req("apt remove foo"), 7).unwrap_err(), ElevateError::Failed(_)));
        assert!(log.records().iter().any(|x| x.op == "elevate" && x.decision == "allow"));
    }

    #[test]
    fn r332_the_tray_approves_through_the_queue_file_and_late_answers_do_not_count() {
        let d = tmp("tray");
        let (k, q, r) = (Kill::open(&d), ElevateQueue::open(&d), Runner::default());
        let s = scope(true);
        // the tray: a thread approving the first pending item it sees
        let q2 = ElevateQueue::open(&d);
        let t = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(p) = q2.list().into_iter().find(|p| p.status == Status::Pending) {
                    q2.approve(&p.id, &LocalApproval::from_machine("tray")).unwrap();
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let qp = QueuePrompt { queue: &q, poll: Duration::from_millis(10) };
        let e = Elevator { scope: &s, kill: &k, queue: &q, prompt: &qp, runner: &r, audit: None, timeout: Duration::from_secs(10) };
        assert!(e.elevate(req("systemctl restart x"), 1).is_ok());
        t.join().unwrap();
        assert_eq!(q.list()[0].decided_by.as_deref(), Some("tray"));
        // nobody answers: the queue prompt gives up at the timeout
        let e = Elevator { scope: &s, kill: &k, queue: &q, prompt: &qp, runner: &r, audit: None, timeout: Duration::from_millis(100) };
        assert!(matches!(e.elevate(req("rm -rf /etc/x"), 2).unwrap_err(), ElevateError::Refused(_)));
        assert_eq!(r.ran.borrow().len(), 1);
        assert_eq!(q.list().last().unwrap().status, Status::Expired);
        // a hook that answers Approve after the deadline is a refusal
        struct Slow;
        impl Prompt for Slow {
            fn ask(&self, _p: &Pending, _t: Duration) -> Answer {
                std::thread::sleep(Duration::from_millis(800));
                Answer::Approve
            }
        }
        let e = Elevator { scope: &s, kill: &k, queue: &q, prompt: &Slow, runner: &r, audit: None, timeout: Duration::from_millis(100) };
        assert!(e.elevate(req("late"), 3).is_err());
        assert_eq!(r.ran.borrow().len(), 1);
    }

    #[test]
    fn r332_the_app_does_not_run_elevated() {
        assert!(check_elevated(false, false).is_ok());
        assert!(check_elevated(false, true).is_ok());
        let e = check_elevated(true, false).unwrap_err();
        assert!(e.contains("refusing to serve") && e.contains(ALLOW_ELEVATED_FLAG));
        assert!(check_elevated(true, true).is_ok(), "the documented expert override");
        assert!(refuse_if_elevated(&[ALLOW_ELEVATED_FLAG.to_string()]).is_ok());
        // the real check, whoever runs the tests
        assert_eq!(refuse_if_elevated(&[]).is_err(), is_elevated());
    }
}
