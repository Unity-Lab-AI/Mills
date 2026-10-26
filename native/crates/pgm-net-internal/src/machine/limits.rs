//! Rate limits on the machine (spec 7.3, R332): a token bucket per tool class, so a looping agent is slowed at the
//! machine itself, whatever the hub does. Over the limit a call is **refused** with "rate limit; try again in N s",
//! never queued. The clock is passed in (milliseconds), so tests need no sleeping.
//!
//! - Classes: [`Class::File`] (list, read, write, delete ...), [`Class::Shell`] (shell_run, job_*, proc_*),
//!   [`Class::Net`] (fetch), [`Class::Transfer`] (transfer_put / transfer_get).
//! - Defaults ([`Limits::default`], per minute, bucket holds one minute): file 300, shell 60, net 30, transfer 20.
//!   Scope `limits.callsPerMin` (default 120) is a second bucket over **all** calls; `limits` may also give
//!   `perClass: {file, shell, net, transfer}` ([`Limits::from_scope_json`]).
//! - [`BurstGuard`]: more than 50 deletes or writes in a minute asks for a confirm on the machine even when allowed
//!   (the scope's `confirm` threshold).
//! - Left to the caller: MB per minute and concurrent jobs (the job table owns those counts).

use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    File,
    Shell,
    Net,
    Transfer,
}

impl Class {
    pub const ALL: [Class; 4] = [Class::File, Class::Shell, Class::Net, Class::Transfer];
    fn idx(self) -> usize {
        self as usize
    }
    pub fn name(self) -> &'static str {
        match self {
            Class::File => "file",
            Class::Shell => "shell",
            Class::Net => "net",
            Class::Transfer => "transfer",
        }
    }
    /// The class of a tool name from scope.rs's tool list.
    pub fn of_tool(tool: &str) -> Class {
        match tool {
            "shell_run" | "job_start" | "job_read" | "job_stop" | "proc_list" | "proc_kill" | "elevate" | "re_run" | "re_stop" | "re_list" | "re_logs" => Class::Shell,
            "fetch" => Class::Net,
            "transfer_put" | "transfer_get" => Class::Transfer,
            _ => Class::File,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// Calls per minute for each class, indexed by [`Class`].
    pub per_class: [u32; 4],
    /// Calls per minute over all classes.
    pub calls_per_min: u32,
    /// Writes + deletes in a minute before a confirm is asked.
    pub confirm_burst: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { per_class: [300, 60, 30, 20], calls_per_min: 120, confirm_burst: 50 }
    }
}

impl Limits {
    /// Read `limits` (and `confirm.burst`) from a scope document; anything missing keeps its default; a zero or a
    /// non-number is ignored (a limit of 0 would lock the machine; use the kill switch for that).
    pub fn from_scope_json(json: &str) -> Limits {
        let mut l = Limits::default();
        let Ok(v) = serde_json::from_str::<Value>(json) else { return l };
        let n = |x: Option<&Value>| x.and_then(|x| x.as_u64()).filter(|n| *n > 0).map(|n| n.min(u32::MAX as u64) as u32);
        if let Some(x) = n(v.pointer("/limits/callsPerMin")) {
            l.calls_per_min = x;
        }
        for c in Class::ALL {
            if let Some(x) = n(v.pointer(&format!("/limits/perClass/{}", c.name()))) {
                l.per_class[c.idx()] = x;
            }
        }
        if let Some(x) = n(v.pointer("/confirm/burst")) {
            l.confirm_burst = x;
        }
        l
    }
}

/// Over the limit: try again in `retry_after_secs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimited {
    pub class: Class,
    pub retry_after_secs: u64,
    /// `true` when the all-calls bucket said it.
    pub overall: bool,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rate limit; try again in {} s", self.retry_after_secs)
    }
}
impl std::error::Error for RateLimited {}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    /// Tokens held (capacity = the per-minute number).
    tokens: f64,
    at_ms: u64,
    started: bool,
}

impl Bucket {
    fn new() -> Bucket {
        Bucket { tokens: 0.0, at_ms: 0, started: false }
    }
    fn refill(&mut self, per_min: u32, now_ms: u64) {
        let cap = per_min as f64;
        if !self.started {
            self.tokens = cap;
            self.started = true;
        } else {
            let dt = now_ms.saturating_sub(self.at_ms) as f64;
            self.tokens = (self.tokens + dt * cap / 60_000.0).min(cap);
        }
        self.at_ms = self.at_ms.max(now_ms);
    }
    /// Seconds until one token exists (rounded up, at least 1).
    fn wait_secs(&self, per_min: u32) -> u64 {
        let need = 1.0 - self.tokens;
        ((need * 60.0 / per_min as f64).ceil() as u64).max(1)
    }
}

pub struct Limiter {
    limits: Limits,
    st: Mutex<([Bucket; 4], Bucket)>,
}

impl Limiter {
    pub fn new(limits: Limits) -> Limiter {
        Limiter { limits, st: Mutex::new(([Bucket::new(); 4], Bucket::new())) }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Take one call for `class` at `now_ms`. A refused call takes nothing from either bucket.
    pub fn check(&self, class: Class, now_ms: u64) -> Result<(), RateLimited> {
        let mut g = self.st.lock().unwrap_or_else(|p| p.into_inner());
        let (classes, all) = &mut *g;
        let pc = self.limits.per_class[class.idx()].max(1);
        let pa = self.limits.calls_per_min.max(1);
        let mut c = classes[class.idx()];
        let mut a = *all;
        c.refill(pc, now_ms);
        a.refill(pa, now_ms);
        if c.tokens < 1.0 {
            classes[class.idx()] = c;
            return Err(RateLimited { class, retry_after_secs: c.wait_secs(pc), overall: false });
        }
        if a.tokens < 1.0 {
            classes[class.idx()] = c;
            *all = a;
            return Err(RateLimited { class, retry_after_secs: a.wait_secs(pa), overall: true });
        }
        c.tokens -= 1.0;
        a.tokens -= 1.0;
        classes[class.idx()] = c;
        *all = a;
        Ok(())
    }

    pub fn check_tool(&self, tool: &str, now_ms: u64) -> Result<(), RateLimited> {
        self.check(Class::of_tool(tool), now_ms)
    }
}

/// Counts writes and deletes in a sliding minute.
pub struct BurstGuard {
    threshold: u32,
    st: Mutex<VecDeque<u64>>,
}

impl BurstGuard {
    pub fn new(threshold: u32) -> BurstGuard {
        BurstGuard { threshold: threshold.max(1), st: Mutex::new(VecDeque::new()) }
    }
    /// Record one write / delete; `true` means "ask the person on the machine to confirm before this one".
    pub fn note_change(&self, now_ms: u64) -> bool {
        let mut q = self.st.lock().unwrap_or_else(|p| p.into_inner());
        while q.front().is_some_and(|t| now_ms.saturating_sub(*t) >= 60_000) {
            q.pop_front();
        }
        q.push_back(now_ms);
        q.len() as u32 > self.threshold
    }
    /// After the person confirmed: start counting again.
    pub fn reset(&self) {
        self.st.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r332_over_limit_calls_are_refused_with_a_retry_after() {
        let l = Limiter::new(Limits { per_class: [10, 2, 1, 1], calls_per_min: 1000, confirm_burst: 50 });
        for _ in 0..10 {
            assert!(l.check(Class::File, 0).is_ok());
        }
        let e = l.check(Class::File, 0).unwrap_err();
        assert_eq!(e.class, Class::File);
        assert_eq!(e.retry_after_secs, 6, "10 a minute is one every 6 s");
        assert_eq!(e.to_string(), "rate limit; try again in 6 s");
        // a refused call took nothing: 6 s later exactly one call fits
        assert!(l.check(Class::File, 6_000).is_ok());
        assert!(l.check(Class::File, 6_000).is_err());
        // the other classes are untouched
        assert!(l.check(Class::Shell, 0).is_ok());
        assert!(l.check(Class::Shell, 0).is_ok());
        assert_eq!(l.check(Class::Shell, 0).unwrap_err().retry_after_secs, 30);
        assert!(l.check(Class::Net, 0).is_ok());
        assert!(l.check(Class::Net, 1).is_err());
        assert!(l.check(Class::Transfer, 0).is_ok());
        // a full minute later the bucket is full again, never above its capacity
        for _ in 0..10 {
            assert!(l.check(Class::File, 10 * 60_000).is_ok());
        }
        assert!(l.check(Class::File, 10 * 60_000).is_err());
    }

    #[test]
    fn r332_the_all_calls_bucket_and_defaults_and_config() {
        let l = Limiter::new(Limits { per_class: [100, 100, 100, 100], calls_per_min: 3, confirm_burst: 50 });
        assert!(l.check(Class::File, 0).is_ok());
        assert!(l.check(Class::Shell, 0).is_ok());
        assert!(l.check(Class::Net, 0).is_ok());
        let e = l.check(Class::Transfer, 0).unwrap_err();
        assert!(e.overall);
        assert_eq!(e.retry_after_secs, 20);
        let d = Limits::default();
        assert_eq!((d.per_class, d.calls_per_min, d.confirm_burst), ([300, 60, 30, 20], 120, 50));
        let c = Limits::from_scope_json(r#"{"limits":{"callsPerMin":10,"perClass":{"shell":5,"net":0,"file":"x"}},"confirm":{"burst":7}}"#);
        assert_eq!((c.calls_per_min, c.per_class, c.confirm_burst), (10, [300, 5, 30, 20], 7));
        assert_eq!(Limits::from_scope_json("not json"), d);
        assert_eq!(Class::of_tool("shell_run"), Class::Shell);
        assert_eq!(Class::of_tool("fetch"), Class::Net);
        assert_eq!(Class::of_tool("transfer_get"), Class::Transfer);
        assert_eq!(Class::of_tool("write"), Class::File);
    }

    #[test]
    fn r332_a_burst_of_writes_or_deletes_asks_for_a_confirm() {
        let g = BurstGuard::new(50);
        for i in 0..50 {
            assert!(!g.note_change(i * 100), "{i}");
        }
        assert!(g.note_change(5_100), "the 51st in a minute");
        assert!(g.note_change(5_200));
        g.reset();
        assert!(!g.note_change(5_300));
        // a slow trickle never trips it
        let g = BurstGuard::new(3);
        for i in 0..20 {
            assert!(!g.note_change(i * 30_000));
        }
    }
}
