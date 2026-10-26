//! The dispatch tool's policy (DECISIONS A1 items 4 and 5): `pgm_dispatch { kind, brief, inputs }` routes `auto:<kind>`
//! through the profile to a worker, and a failed or refused unit is re-submitted to the next worker, bounded by
//! `Profile::max_attempts`, every attempt on the timeline. Pure: the caller runs the worker (a [`Runner`]) or hands the
//! [`Plan`] to the model to run with the swarm tool, and reports the outcome with [`record`].
use crate::attempts::{next_attempt, Attempt, AttemptOutcome, NextStep, Timeline, Unit};
use crate::health::{classify_failure, Health, Outcome};
use crate::profile::{Profile, TaskKind};
use crate::select::{infer_task_kind, select};

/// A stable unit id from the brief (whitespace-normalised FNV-1a), so the same brief sent again lands on the same
/// timeline and the next worker is chosen.
pub fn unit_id_for(brief: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in brief
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .bytes()
    {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("u-{:08x}", h as u32)
}

/// `"implement"`, `"auto:implement"`, `"auto"` or `""` (the last two read the kind off the brief).
pub fn parse_kind(kind: &str, brief: &str) -> Result<TaskKind, DispatchError> {
    let k = kind.trim();
    let k = k
        .strip_prefix("auto:")
        .or_else(|| k.strip_prefix("AUTO:"))
        .unwrap_or(k);
    if k.is_empty() || k.eq_ignore_ascii_case("auto") {
        return Ok(infer_task_kind(brief));
    }
    TaskKind::parse(k).ok_or_else(|| DispatchError::UnknownKind {
        given: kind.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    UnknownKind {
        given: String,
    },
    /// the unit already succeeded; send a new unit id to run it again
    AlreadyDone {
        unit: String,
        worker: String,
    },
    /// no worker of the profile can take this brief (never the orchestrator's own model)
    Unroutable {
        profile: String,
        kind: TaskKind,
        brief_tokens: usize,
        reasons: Vec<String>,
    },
    /// attempts used up, or no untried worker left
    GaveUp {
        reason: String,
    },
}

impl DispatchError {
    pub fn message(&self) -> String {
        match self {
            DispatchError::UnknownKind { given } => format!(
                "unknown kind {given:?}; use one of {} (or auto)",
                TaskKind::ALL.map(|k| k.as_str()).join(", ")
            ),
            DispatchError::AlreadyDone { unit, worker } => format!(
                "unit {unit} already succeeded on {worker}; give inputs.unit a new id to run it again"
            ),
            DispatchError::Unroutable { profile, kind, brief_tokens, reasons } => format!(
                "no worker in profile {profile} can take a {} brief of ~{brief_tokens} tokens ({}); split the brief or pick another kind, never your own model",
                kind.as_str(),
                if reasons.is_empty() { "no admitted workers".to_string() } else { reasons.join("; ") }
            ),
            DispatchError::GaveUp { reason } => reason.clone(),
        }
    }
}

/// What to run next: this worker, as attempt `attempt` of at most `max_attempts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub unit: Unit,
    pub worker: String,
    pub attempt: u32,
    pub max_attempts: u32,
    /// workers already tried for this unit, in order
    pub tried: Vec<String>,
}

/// Route a unit. The first call of a brief picks the best worker; a call for a unit with failed attempts picks the
/// next untried one (benches and refusals apply), until `max_attempts` is used up.
pub fn plan(
    profile: &Profile,
    health: &Health,
    timeline: &Timeline,
    kind: &str,
    brief: &str,
    unit_id: Option<&str>,
    now_ms: u64,
) -> Result<Plan, DispatchError> {
    let kind = parse_kind(kind, brief)?;
    let id = unit_id
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| unit_id_for(brief));
    let unit = Unit::new(&id, kind, brief);
    let tried: Vec<String> = timeline.of(&id).iter().map(|a| a.worker.clone()).collect();
    match next_attempt(profile, health, timeline, &unit, now_ms) {
        NextStep::Submit { worker, attempt } => Ok(Plan {
            unit,
            worker,
            attempt,
            max_attempts: profile.max_attempts.max(1),
            tried,
        }),
        NextStep::Done { worker } => Err(DispatchError::AlreadyDone { unit: id, worker }),
        NextStep::GiveUp { .. } if tried.is_empty() => {
            let sel = select(profile, health, kind, unit.brief_tokens(), now_ms / 1000);
            Err(DispatchError::Unroutable {
                profile: profile.name.clone(),
                kind,
                brief_tokens: unit.brief_tokens(),
                reasons: sel
                    .skipped
                    .iter()
                    .map(|(r, why)| format!("{r}: {why}"))
                    .collect(),
            })
        }
        NextStep::GiveUp { reason } => Err(DispatchError::GaveUp { reason }),
    }
}

fn health_outcome(o: &AttemptOutcome) -> Outcome {
    match o {
        AttemptOutcome::Ok => Outcome::Success,
        AttemptOutcome::Refused { .. } => Outcome::Refused,
        AttemptOutcome::TimedOut => Outcome::Error,
        AttemptOutcome::Failed { reason } => match classify_failure(reason) {
            Outcome::Success | Outcome::Cancelled => Outcome::Error,
            o => o,
        },
    }
}

/// Put an attempt on the timeline and update the worker's health from it. `now_ms` is the end time.
pub fn record(
    profile: &Profile,
    health: &mut Health,
    timeline: &mut Timeline,
    plan: &Plan,
    outcome: AttemptOutcome,
    started_ms: u64,
    now_ms: u64,
    bench_secs: u64,
) {
    health.record(
        &plan.worker,
        plan.unit.kind,
        health_outcome(&outcome),
        now_ms / 1000,
        profile.failures_before_bench,
        bench_secs,
        &outcome.text(),
    );
    timeline.push(Attempt {
        unit_id: plan.unit.id.clone(),
        attempt: plan.attempt,
        worker: plan.worker.clone(),
        outcome,
        started_ms,
        ended_ms: now_ms.max(started_ms),
    });
}

/// Runs one attempt on the planned worker.
pub trait Runner {
    fn run(&mut self, plan: &Plan) -> (AttemptOutcome, String);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub worker: String,
    pub attempt: u32,
    pub text: String,
}

/// Plan, run, record and (on a failure) re-submit to the next worker until one succeeds or the attempts are used up.
/// `clock` gives unix milliseconds. Returns the unit's id with the result.
#[allow(clippy::too_many_arguments)]
pub fn run(
    profile: &Profile,
    health: &mut Health,
    timeline: &mut Timeline,
    kind: &str,
    brief: &str,
    unit_id: Option<&str>,
    bench_secs: u64,
    clock: &mut dyn FnMut() -> u64,
    runner: &mut dyn Runner,
) -> Result<Done, DispatchError> {
    loop {
        let p = plan(profile, health, timeline, kind, brief, unit_id, clock())?;
        health.started(&p.worker);
        let started = clock();
        let (outcome, text) = runner.run(&p);
        let ok = outcome.is_ok();
        record(profile, health, timeline, &p, outcome, started, clock(), bench_secs);
        if ok {
            return Ok(Done {
                worker: p.worker,
                attempt: p.attempt,
                text,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Mode, Worker};

    fn profile(max: u32) -> Profile {
        let mut p = Profile::new(
            "t",
            Mode::Mixed,
            ["a", "b", "c", "d"]
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    Worker::new(&format!("openrouter:{n}:free"))
                        .with(100_000, &[TaskKind::Implement, TaskKind::General], true)
                        .rank(i as u32 + 1)
                })
                .collect(),
        );
        p.max_attempts = max;
        p
    }

    struct Script(Vec<(AttemptOutcome, &'static str)>, Vec<String>);
    impl Runner for Script {
        fn run(&mut self, plan: &Plan) -> (AttemptOutcome, String) {
            self.1.push(plan.worker.clone());
            let (o, t) = self.0.remove(0);
            (o, t.to_string())
        }
    }
    fn failed(r: &str) -> AttemptOutcome {
        AttemptOutcome::Failed { reason: r.into() }
    }
    fn tick() -> impl FnMut() -> u64 {
        let mut t = 1_000_000u64;
        move || {
            t += 10;
            t
        }
    }

    #[test]
    fn kinds_parse_from_auto_forms_and_the_brief() {
        let b = "Fix the panic";
        assert_eq!(parse_kind("auto:review", b), Ok(TaskKind::Review));
        assert_eq!(parse_kind("implement", b), Ok(TaskKind::Implement));
        assert_eq!(parse_kind("auto", b), Ok(TaskKind::Debug));
        assert_eq!(parse_kind("", "hello"), Ok(TaskKind::General));
        let e = parse_kind("auto:bogus", b).unwrap_err();
        assert!(e.message().contains("implement") && e.message().contains("bogus"));
    }

    #[test]
    fn the_first_call_routes_by_the_profile_and_the_unit_id_is_stable() {
        let p = plan(&profile(3), &Health::new(), &Timeline::new(), "auto:implement", "write  the\nparser", None, 0).unwrap();
        assert_eq!((p.worker.as_str(), p.attempt, p.max_attempts), ("openrouter:a:free", 1, 3));
        assert_eq!(p.unit.id, unit_id_for("write the parser"));
        assert!(p.unit.id.starts_with("u-") && p.tried.is_empty());
        let named = plan(&profile(3), &Health::new(), &Timeline::new(), "implement", "x", Some("mine"), 0).unwrap();
        assert_eq!(named.unit.id, "mine");
    }

    #[test]
    fn unroutable_lists_every_reason_and_never_names_the_own_model() {
        let huge = "x".repeat(2_000_000);
        let e = plan(&profile(3), &Health::new(), &Timeline::new(), "implement", &huge, None, 0).unwrap_err();
        let m = e.message();
        assert!(matches!(e, DispatchError::Unroutable { .. }), "{m}");
        assert!(m.contains("openrouter:a:free: brief") && m.contains("openrouter:d:free") && m.contains("never your own model"), "{m}");
        let none = plan(&Profile::new("e", Mode::Mixed, vec![]), &Health::new(), &Timeline::new(), "implement", "x", None, 0).unwrap_err();
        assert!(none.message().contains("no admitted workers"));
    }

    #[test]
    fn a_failed_unit_goes_to_the_next_untried_worker_and_every_attempt_is_recorded() {
        let (p, mut h, mut tl) = (profile(3), Health::new(), Timeline::new());
        let mut runner = Script(vec![(failed("process exited with code 1"), ""), (AttemptOutcome::Refused { reason: "I can't help".into() }, ""), (AttemptOutcome::Ok, "done: parser written")], vec![]);
        let done = run(&p, &mut h, &mut tl, "implement", "write the parser", None, 600, &mut tick(), &mut runner).unwrap();
        assert_eq!((done.worker.as_str(), done.attempt, done.text.as_str()), ("openrouter:c:free", 3, "done: parser written"));
        assert_eq!(runner.1, ["openrouter:a:free", "openrouter:b:free", "openrouter:c:free"]);
        let id = unit_id_for("write the parser");
        let rows: Vec<(u32, &str, bool)> = tl.of(&id).iter().map(|a| (a.attempt, a.worker.as_str(), a.outcome.is_ok())).collect();
        assert_eq!(rows, [(1, "openrouter:a:free", false), (2, "openrouter:b:free", false), (3, "openrouter:c:free", true)]);
        assert!(tl.of(&id).iter().all(|a| a.duration_ms() > 0));
        // health followed the outcomes: b refused implement for good, c has a success, nobody is in flight
        assert!(h.refuses("openrouter:b:free", TaskKind::Implement) && h.successes("openrouter:c:free") == 1);
        assert!(h.workers.values().all(|w| w.in_flight == 0));
        // the unit is done: sending it again says so instead of spending another worker
        let again = plan(&p, &h, &tl, "implement", "write the parser", None, 2_000_000).unwrap_err();
        assert!(matches!(again, DispatchError::AlreadyDone { ref worker, .. } if worker == "openrouter:c:free"));
    }

    #[test]
    fn re_submission_stops_at_max_attempts_with_the_whole_history() {
        let (p, mut h, mut tl) = (profile(2), Health::new(), Timeline::new());
        let mut runner = Script(vec![(failed("boom"), ""), (failed("bang"), ""), (AttemptOutcome::Ok, "never reached")], vec![]);
        let e = run(&p, &mut h, &mut tl, "implement", "write the lexer", None, 600, &mut tick(), &mut runner).unwrap_err();
        assert!(matches!(e, DispatchError::GaveUp { .. }));
        let m = e.message();
        assert!(m.contains("gave up after 2 of 2 attempts") && m.contains("#1 openrouter:a:free failed: boom") && m.contains("#2 openrouter:b:free failed: bang"), "{m}");
        assert_eq!(runner.1.len(), 2, "the third worker was never asked");
    }

    #[test]
    fn running_out_of_untried_workers_is_a_give_up_not_a_loop() {
        let mut p = profile(10);
        p.workers.truncate(2);
        let (mut h, mut tl) = (Health::new(), Timeline::new());
        let mut runner = Script(vec![(failed("x"), ""), (failed("y"), "")], vec![]);
        let e = run(&p, &mut h, &mut tl, "implement", "brief", None, 600, &mut tick(), &mut runner).unwrap_err();
        assert!(e.message().contains("no worker left to try"), "{}", e.message());
    }

    #[test]
    fn a_403_failure_benches_the_worker_for_the_next_unit_too() {
        let (p, mut h, mut tl) = (profile(3), Health::new(), Timeline::new());
        let mut runner = Script(vec![(failed("HTTP 403 forbidden"), ""), (AttemptOutcome::Ok, "ok")], vec![]);
        let clock_start = 1_000_000;
        run(&p, &mut h, &mut tl, "implement", "one", None, 600, &mut tick(), &mut runner).unwrap();
        assert!(h.benched("openrouter:a:free", clock_start / 1000 + 5));
        let next = plan(&p, &h, &tl, "implement", "another brief", None, clock_start + 100).unwrap();
        assert_eq!(next.worker, "openrouter:b:free");
    }
}
