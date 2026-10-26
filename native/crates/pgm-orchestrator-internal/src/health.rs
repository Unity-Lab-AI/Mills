//! Health per worker. Unavailable routes and refusals bench on the first strike (retrying is pointless); generic
//! errors take `failures_before_bench` strikes; a refusal also steers that task kind away from the worker for good
//! (rule 3). A success clears strikes and any bench. Our own cancellations are not strikes (rule 4).
use crate::profile::TaskKind;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// the model answered but declined the task (guardrail, "I can't help with…")
    Refused,
    /// 403 / 404 / "temporarily rate-limited" / not served: the route itself is not available
    Unavailable,
    /// any other failure (timeout, bad output, tool errors)
    Error,
    /// we stopped it ourselves (reaped, reassigned)
    Cancelled,
}

/// Classifies a worker's failure text the way the fork learned to (403 and "temporarily rate-limited" are route
/// refusals, not content refusals; both bench on first strike).
pub fn classify_failure(text: &str) -> Outcome {
    let t = text.to_ascii_lowercase();
    if t.contains("403") || t.contains("404") || t.contains("429") || t.contains("rate limit") || t.contains("rate-limited") || t.contains("not available") || t.contains("no endpoints") || t.contains("unavailable") { return Outcome::Unavailable; }
    if t.contains("i can't help") || t.contains("i cannot help") || t.contains("i won't") || t.contains("against my") || t.contains("refus") || t.contains("not able to assist") { return Outcome::Refused; }
    Outcome::Error
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerHealth {
    pub strikes: u32,
    /// benched until this many seconds since the epoch (0 = not benched)
    pub benched_until: u64,
    pub successes: u32,
    pub failures: u32,
    /// task kinds this worker refused: never again for those (rule 3)
    pub refused_kinds: BTreeSet<TaskKind>,
    /// briefs in flight right now (for spreading)
    pub in_flight: u32,
    pub last_note: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub workers: BTreeMap<String, WorkerHealth>,
}

impl Health {
    pub fn new() -> Health { Health::default() }
    pub fn of(&self, route: &str) -> WorkerHealth { self.workers.get(route).cloned().unwrap_or_default() }
    fn entry(&mut self, route: &str) -> &mut WorkerHealth { self.workers.entry(route.to_string()).or_default() }
    pub fn started(&mut self, route: &str) { self.entry(route).in_flight += 1; }
    /// Record an outcome at `now` (seconds since the epoch). `bench_secs` is the cooldown for a bench.
    pub fn record(&mut self, route: &str, kind: TaskKind, outcome: Outcome, now: u64, failures_before_bench: u32, bench_secs: u64, note: &str) {
        let h = self.entry(route);
        h.in_flight = h.in_flight.saturating_sub(1);
        h.last_note = note.chars().take(200).collect();
        match outcome {
            Outcome::Success => { h.successes += 1; h.strikes = 0; h.benched_until = 0; }
            Outcome::Cancelled => {}
            Outcome::Refused => { h.failures += 1; h.refused_kinds.insert(kind); h.strikes += 1; h.benched_until = now + bench_secs; }
            Outcome::Unavailable => { h.failures += 1; h.strikes += 1; h.benched_until = now + bench_secs; }
            Outcome::Error => { h.failures += 1; h.strikes += 1; if h.strikes >= failures_before_bench.max(1) { h.benched_until = now + bench_secs; } }
        }
    }
    pub fn benched(&self, route: &str, now: u64) -> bool { self.of(route).benched_until > now }
    pub fn refuses(&self, route: &str, kind: TaskKind) -> bool { self.of(route).refused_kinds.contains(&kind) }
    pub fn load(&self, route: &str) -> u32 { self.of(route).in_flight }
    pub fn successes(&self, route: &str) -> u32 { self.of(route).successes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_and_unavailable_bench_at_once_errors_take_strikes_successes_clear() {
        let mut h = Health::new();
        h.started("w"); h.record("w", TaskKind::Implement, Outcome::Error, 100, 2, 60, "timeout");
        assert!(!h.benched("w", 101), "one generic error is not a bench");
        h.record("w", TaskKind::Implement, Outcome::Error, 102, 2, 60, "bad output");
        assert!(h.benched("w", 103) && !h.benched("w", 170));
        h.record("w", TaskKind::Implement, Outcome::Success, 200, 2, 60, "ok");
        assert!(!h.benched("w", 201) && h.of("w").strikes == 0 && h.successes("w") == 1);
        h.record("w", TaskKind::Review, Outcome::Refused, 300, 2, 60, "I can't help with that");
        assert!(h.benched("w", 301) && h.refuses("w", TaskKind::Review) && !h.refuses("w", TaskKind::Implement));
        h.record("w", TaskKind::Review, Outcome::Success, 400, 2, 60, "fine");
        assert!(h.refuses("w", TaskKind::Review), "a refusal is permanent for that kind (rule 3)");
        h.record("v", TaskKind::General, Outcome::Unavailable, 500, 2, 60, "403");
        assert!(h.benched("v", 501));
        h.started("c"); h.record("c", TaskKind::General, Outcome::Cancelled, 600, 2, 60, "reaped");
        assert_eq!(h.of("c").strikes, 0); assert_eq!(h.load("c"), 0);
    }

    #[test]
    fn failure_text_is_classified_like_the_fork_learned() {
        assert_eq!(classify_failure("HTTP 403: only available on agentic harnesses"), Outcome::Unavailable);
        assert_eq!(classify_failure("temporarily rate-limited upstream"), Outcome::Unavailable);
        assert_eq!(classify_failure("I can't help with modifying that"), Outcome::Refused);
        assert_eq!(classify_failure("process exited with code 1"), Outcome::Error);
    }
}
