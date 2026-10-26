//! Re-submission and the per-unit timeline (DECISIONS A1 item 5). A unit of work whose attempt failed, was refused or
//! timed out is handed to the next fallback worker, bounded by the profile's `max_attempts`, never to a worker that
//! already failed this unit. The timeline is what `/pgm-orchestrator timeline` shows. Pure: time is unix ms.
use crate::health::Health;
use crate::profile::{Profile, TaskKind};
use crate::select::select;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    pub id: String,
    pub kind: TaskKind,
    pub brief: String,
}

impl Unit {
    pub fn new(id: &str, kind: TaskKind, brief: &str) -> Unit {
        Unit {
            id: id.into(),
            kind,
            brief: brief.into(),
        }
    }
    /// A rough token count of the brief (4 characters a token, rounded up): enough for the window check.
    pub fn brief_tokens(&self) -> usize {
        self.brief.chars().count().div_ceil(4)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Ok,
    Failed { reason: String },
    Refused { reason: String },
    TimedOut,
}

impl AttemptOutcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, AttemptOutcome::Ok)
    }
    pub fn text(&self) -> String {
        match self {
            AttemptOutcome::Ok => "ok".into(),
            AttemptOutcome::Failed { reason } => format!("failed: {reason}"),
            AttemptOutcome::Refused { reason } => format!("refused: {reason}"),
            AttemptOutcome::TimedOut => "timed out".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub unit_id: String,
    /// 1-based
    pub attempt: u32,
    pub worker: String,
    pub outcome: AttemptOutcome,
    pub started_ms: u64,
    pub ended_ms: u64,
}

impl Attempt {
    pub fn duration_ms(&self) -> u64 {
        self.ended_ms.saturating_sub(self.started_ms)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeline {
    pub attempts: Vec<Attempt>,
}

impl Timeline {
    pub fn new() -> Timeline {
        Timeline::default()
    }
    pub fn push(&mut self, a: Attempt) {
        self.attempts.push(a);
    }
    /// The attempts of one unit, in attempt order.
    pub fn of(&self, unit_id: &str) -> Vec<&Attempt> {
        let mut v: Vec<&Attempt> = self
            .attempts
            .iter()
            .filter(|a| a.unit_id == unit_id)
            .collect();
        v.sort_by_key(|a| (a.attempt, a.started_ms));
        v
    }
    /// A compact text table: `attempt | worker | outcome | ms`.
    pub fn render(&self, unit_id: &str) -> String {
        let v = self.of(unit_id);
        if v.is_empty() {
            return format!("timeline {unit_id}: no attempts");
        }
        let mut out = format!("timeline {unit_id}\nattempt | worker | outcome | ms\n");
        for a in &v {
            out.push_str(&format!(
                "{} | {} | {} | {}\n",
                a.attempt,
                a.worker,
                a.outcome.text(),
                a.duration_ms()
            ));
        }
        out.pop();
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextStep {
    /// submit the unit to `worker` as attempt number `attempt` (1-based)
    Submit { worker: String, attempt: u32 },
    /// the unit already succeeded on `worker`: nothing to submit (an addition to the item-5 spec)
    Done { worker: String },
    /// stop; the reason lists every attempt
    GiveUp { reason: String },
}

fn listing(attempts: &[&Attempt]) -> String {
    attempts
        .iter()
        .map(|a| format!("#{} {} {}", a.attempt, a.worker, a.outcome.text()))
        .collect::<Vec<_>>()
        .join("; ")
}

/// What to do with a unit next. `now_ms` feeds the health benches (health keeps seconds).
pub fn next_attempt(
    profile: &Profile,
    health: &Health,
    timeline: &Timeline,
    unit: &Unit,
    now_ms: u64,
) -> NextStep {
    let tried = timeline.of(&unit.id);
    if let Some(ok) = tried.iter().find(|a| a.outcome.is_ok()) {
        return NextStep::Done {
            worker: ok.worker.clone(),
        };
    }
    let max = profile.max_attempts.max(1);
    if tried.len() as u32 >= max {
        return NextStep::GiveUp {
            reason: format!(
                "unit {} ({}): gave up after {} of {} attempts: {}",
                unit.id,
                unit.kind.as_str(),
                tried.len(),
                max,
                listing(&tried)
            ),
        };
    }
    let sel = select(
        profile,
        health,
        unit.kind,
        unit.brief_tokens(),
        now_ms / 1000,
    );
    match sel
        .routes
        .iter()
        .find(|r| !tried.iter().any(|a| &a.worker == *r))
    {
        Some(w) => NextStep::Submit {
            worker: w.clone(),
            attempt: tried.len() as u32 + 1,
        },
        None => {
            let skipped = sel
                .skipped
                .iter()
                .map(|(r, why)| format!("{r} ({why})"))
                .collect::<Vec<_>>()
                .join(", ");
            let history = if tried.is_empty() {
                "no attempts yet".to_string()
            } else {
                listing(&tried)
            };
            NextStep::GiveUp {
                reason: format!(
                    "unit {} ({}): no worker left to try. attempts: {}{}",
                    unit.id,
                    unit.kind.as_str(),
                    history,
                    if skipped.is_empty() {
                        String::new()
                    } else {
                        format!("; skipped: {skipped}")
                    }
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Mode, Worker};

    fn profile() -> Profile {
        Profile::new(
            "t",
            Mode::Mixed,
            vec![
                Worker::new("openrouter:a:free")
                    .with(100_000, &[TaskKind::Implement, TaskKind::General], true)
                    .rank(1),
                Worker::new("openrouter:b:free")
                    .with(100_000, &[TaskKind::Implement, TaskKind::General], true)
                    .rank(2),
                Worker::new("openrouter:c:free")
                    .with(100_000, &[TaskKind::Implement, TaskKind::General], true)
                    .rank(3),
                Worker::new("openrouter:d:free")
                    .with(100_000, &[TaskKind::Implement, TaskKind::General], true)
                    .rank(4),
            ],
        )
    }
    fn unit() -> Unit {
        Unit::new("u1", TaskKind::Implement, "write the parser")
    }
    fn att(n: u32, w: &str, o: AttemptOutcome) -> Attempt {
        Attempt {
            unit_id: "u1".into(),
            attempt: n,
            worker: format!("openrouter:{w}:free"),
            outcome: o,
            started_ms: 1_000 * n as u64,
            ended_ms: 1_000 * n as u64 + 250,
        }
    }
    fn failed(r: &str) -> AttemptOutcome {
        AttemptOutcome::Failed { reason: r.into() }
    }

    #[test]
    fn the_first_attempt_goes_to_the_selected_worker() {
        let t = Timeline::new();
        assert_eq!(
            next_attempt(&profile(), &Health::new(), &t, &unit(), 0),
            NextStep::Submit {
                worker: "openrouter:a:free".into(),
                attempt: 1
            }
        );
    }

    #[test]
    fn a_failure_resubmits_to_the_next_fallback_never_the_same_worker() {
        let mut t = Timeline::new();
        t.push(att(1, "a", failed("bad output")));
        assert_eq!(
            next_attempt(&profile(), &Health::new(), &t, &unit(), 0),
            NextStep::Submit {
                worker: "openrouter:b:free".into(),
                attempt: 2
            }
        );
        t.push(att(2, "b", AttemptOutcome::TimedOut));
        assert_eq!(
            next_attempt(&profile(), &Health::new(), &t, &unit(), 0),
            NextStep::Submit {
                worker: "openrouter:c:free".into(),
                attempt: 3
            }
        );
    }

    #[test]
    fn refusals_and_timeouts_also_exclude_the_worker() {
        let mut t = Timeline::new();
        t.push(att(
            1,
            "a",
            AttemptOutcome::Refused {
                reason: "403".into(),
            },
        ));
        t.push(att(2, "b", AttemptOutcome::TimedOut));
        match next_attempt(&profile(), &Health::new(), &t, &unit(), 0) {
            NextStep::Submit { worker, .. } => assert!(worker.ends_with("c:free")),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn attempts_are_bounded_by_the_profile_and_give_up_lists_every_attempt() {
        let mut t = Timeline::new();
        t.push(att(1, "a", failed("x")));
        t.push(att(2, "b", AttemptOutcome::TimedOut));
        t.push(att(
            3,
            "c",
            AttemptOutcome::Refused {
                reason: "no".into(),
            },
        ));
        let NextStep::GiveUp { reason } = next_attempt(&profile(), &Health::new(), &t, &unit(), 0)
        else {
            panic!("should give up at the bound")
        };
        for w in [
            "a:free",
            "b:free",
            "c:free",
            "failed: x",
            "timed out",
            "refused: no",
            "3 of 3",
        ] {
            assert!(reason.contains(w), "{w} missing from {reason}");
        }
        assert!(!reason.contains("d:free"), "d was never tried");
        // a larger bound lets the fourth worker go
        let mut p = profile();
        p.max_attempts = 4;
        assert_eq!(
            next_attempt(&p, &Health::new(), &t, &unit(), 0),
            NextStep::Submit {
                worker: "openrouter:d:free".into(),
                attempt: 4
            }
        );
    }

    #[test]
    fn running_out_of_workers_gives_up_before_the_bound_with_the_history() {
        let mut p = profile();
        p.workers.truncate(2);
        p.max_attempts = 5;
        let mut t = Timeline::new();
        t.push(att(1, "a", failed("e1")));
        t.push(att(2, "b", failed("e2")));
        let NextStep::GiveUp { reason } = next_attempt(&p, &Health::new(), &t, &unit(), 0) else {
            panic!()
        };
        assert!(
            reason.contains("no worker left")
                && reason.contains("#1 openrouter:a:free failed: e1")
                && reason.contains("#2 openrouter:b:free failed: e2"),
            "{reason}"
        );
    }

    #[test]
    fn benched_workers_are_skipped_using_the_clock_in_ms() {
        let mut h = Health::new();
        h.record(
            "openrouter:a:free",
            TaskKind::Implement,
            crate::health::Outcome::Unavailable,
            10,
            2,
            600,
            "403",
        );
        let t = Timeline::new();
        assert_eq!(
            next_attempt(&profile(), &h, &t, &unit(), 11_000),
            NextStep::Submit {
                worker: "openrouter:b:free".into(),
                attempt: 1
            }
        );
        assert_eq!(
            next_attempt(&profile(), &h, &t, &unit(), 700_000),
            NextStep::Submit {
                worker: "openrouter:a:free".into(),
                attempt: 1
            }
        );
    }

    #[test]
    fn a_succeeded_unit_is_done_and_other_units_do_not_interfere() {
        let mut t = Timeline::new();
        t.push(att(1, "a", AttemptOutcome::Ok));
        assert_eq!(
            next_attempt(&profile(), &Health::new(), &t, &unit(), 0),
            NextStep::Done {
                worker: "openrouter:a:free".into()
            }
        );
        let other = Unit::new("u2", TaskKind::Implement, "other");
        assert_eq!(
            next_attempt(&profile(), &Health::new(), &t, &other, 0),
            NextStep::Submit {
                worker: "openrouter:a:free".into(),
                attempt: 1
            }
        );
    }

    #[test]
    fn an_oversized_brief_gives_up_with_the_skip_reasons() {
        let big = Unit::new("u1", TaskKind::Implement, &"x".repeat(4 * 500_000));
        let NextStep::GiveUp { reason } =
            next_attempt(&profile(), &Health::new(), &Timeline::new(), &big, 0)
        else {
            panic!()
        };
        assert!(
            reason.contains("no attempts yet") && reason.contains("budget"),
            "{reason}"
        );
    }

    #[test]
    fn the_timeline_renders_a_compact_table_in_attempt_order() {
        let mut t = Timeline::new();
        t.push(att(2, "b", AttemptOutcome::Ok));
        t.push(att(1, "a", failed("boom")));
        t.push(Attempt {
            unit_id: "other".into(),
            ..att(1, "c", AttemptOutcome::Ok)
        });
        let r = t.render("u1");
        assert_eq!(r, "timeline u1\nattempt | worker | outcome | ms\n1 | openrouter:a:free | failed: boom | 250\n2 | openrouter:b:free | ok | 250");
        assert_eq!(t.render("nope"), "timeline nope: no attempts");
    }

    #[test]
    fn old_profile_configs_without_max_attempts_still_parse_with_three() {
        let mut v = serde_json::to_value(profile()).unwrap();
        v.as_object_mut().unwrap().remove("max_attempts");
        for w in v["workers"].as_array_mut().unwrap() {
            for k in [
                "prompt_price_micros_per_m",
                "completion_price_micros_per_m",
                "cache_read_price_micros_per_m",
                "cache_write_price_micros_per_m",
            ] {
                w.as_object_mut().unwrap().remove(k);
            }
        }
        let p: Profile = serde_json::from_value(v).unwrap();
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.workers[0].prompt_price_micros_per_m, None);
    }
}
