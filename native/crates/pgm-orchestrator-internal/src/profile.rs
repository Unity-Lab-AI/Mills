//! Profiles: who does the work. The user lists workers; the profile says which of them may be used (free only, one
//! provider only, paid only, a mix). Every worker carries an exact model route (`provider:model` as the harness
//! spells it), the context window the provider reports (never fabricated: a missing window excludes the worker), the
//! task kinds it is good at, a rank, whether it is free, and a trust level.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Research,
    Summarize,
    Implement,
    Verify,
    Debug,
    Review,
    Write,
    General,
}

impl TaskKind {
    pub const ALL: [TaskKind; 8] = [
        TaskKind::Research,
        TaskKind::Summarize,
        TaskKind::Implement,
        TaskKind::Verify,
        TaskKind::Debug,
        TaskKind::Review,
        TaskKind::Write,
        TaskKind::General,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Research => "research",
            TaskKind::Summarize => "summarize",
            TaskKind::Implement => "implement",
            TaskKind::Verify => "verify",
            TaskKind::Debug => "debug",
            TaskKind::Review => "review",
            TaskKind::Write => "write",
            TaskKind::General => "general",
        }
    }
    pub fn parse(s: &str) -> Option<TaskKind> {
        TaskKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s.trim().to_ascii_lowercase())
    }
    /// Whether work of this kind may change the repository.
    pub fn mutates(self) -> bool {
        matches!(
            self,
            TaskKind::Implement | TaskKind::Debug | TaskKind::Write
        )
    }
}

/// What a worker may be handed. Never loosens the harness's own safety gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// research, summaries, reviews: no repo-mutating briefs
    ReadOnly,
    #[default]
    Standard,
    Trusted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    /// The exact route the harness understands, e.g. `openrouter:z-ai/glm-5.2:free`, `claude-api:claude-haiku-5-5`,
    /// `ollama:qwen3-coder:30b`. Copied verbatim into a spawn's `model`.
    pub route: String,
    /// The provider half of the route (what profiles filter on): `openrouter`, `claude-api`, `ollama`, …
    #[serde(default)]
    pub provider: String,
    /// The context window the provider reports. None = unknown = never selected (rule 6).
    #[serde(default)]
    pub context_tokens: Option<usize>,
    #[serde(default)]
    pub good_at: Vec<TaskKind>,
    /// Lower is preferred among equals (after load). Default 100.
    #[serde(default = "default_rank")]
    pub rank: u32,
    #[serde(default)]
    pub free: bool,
    #[serde(default)]
    pub trust: Trust,
    /// A note for the roster briefing (what it is, quirks).
    #[serde(default)]
    pub note: String,
    /// Price per million prompt tokens in USD micros (1_000_000 = $1.00/M). None = unknown (never invented; see
    /// `spend`). Integer micros keep `Worker: Eq`.
    #[serde(default)]
    pub prompt_price_micros_per_m: Option<u64>,
    #[serde(default)]
    pub completion_price_micros_per_m: Option<u64>,
    /// Cache read / write prices per million tokens in USD micros; None = unknown.
    #[serde(default)]
    pub cache_read_price_micros_per_m: Option<u64>,
    #[serde(default)]
    pub cache_write_price_micros_per_m: Option<u64>,
}
fn default_rank() -> u32 {
    100
}

impl Worker {
    pub fn new(route: &str) -> Worker {
        let provider = route.split(':').next().unwrap_or("").to_string();
        Worker {
            route: route.into(),
            provider,
            context_tokens: None,
            good_at: vec![],
            rank: 100,
            free: false,
            trust: Trust::Standard,
            note: String::new(),
            prompt_price_micros_per_m: None,
            completion_price_micros_per_m: None,
            cache_read_price_micros_per_m: None,
            cache_write_price_micros_per_m: None,
        }
    }
    /// Sets the per-million prices (USD micros) for prompt and completion tokens.
    pub fn priced(mut self, prompt_micros_per_m: u64, completion_micros_per_m: u64) -> Worker {
        self.prompt_price_micros_per_m = Some(prompt_micros_per_m);
        self.completion_price_micros_per_m = Some(completion_micros_per_m);
        self
    }
    pub fn with(mut self, context_tokens: usize, good_at: &[TaskKind], free: bool) -> Worker {
        self.context_tokens = Some(context_tokens);
        self.good_at = good_at.to_vec();
        self.free = free;
        self
    }
    pub fn rank(mut self, rank: u32) -> Worker {
        self.rank = rank;
        self
    }
    pub fn trust(mut self, trust: Trust) -> Worker {
        self.trust = trust;
        self
    }
    /// Whether an OpenRouter route targets the free tier: the `:free` suffix is part of the model id, and its paid
    /// twin has the same id without it. A worker marked free whose route does not say so is called out, not trusted.
    pub fn route_says_free(&self) -> bool {
        self.route.ends_with(":free")
            || self.provider == "ollama"
            || self.provider == "llmd"
            || self.provider == "lmstudio"
            || self.provider == "local"
    }
    pub fn covers(&self, kind: TaskKind) -> bool {
        self.good_at.contains(&kind)
            || (kind == TaskKind::General
                && !self.good_at.is_empty()
                && self.good_at.contains(&TaskKind::General))
    }
}

/// Which workers a profile allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "provider")]
pub enum Mode {
    /// only workers that cost nothing (free routes, local models)
    FreeOnly,
    /// only one provider's workers
    ProviderOnly(String),
    /// only paid workers (when free tiers are too flaky for the job at hand)
    PaidOnly,
    /// everything listed
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(flatten)]
    pub mode: Mode,
    pub workers: Vec<Worker>,
    /// A brief may use at most this share of a worker's window… (default 0.6)
    #[serde(default = "default_fraction")]
    pub brief_context_fraction: f64,
    /// …and must leave at least this many tokens free (default 8,000). The stricter bound wins.
    #[serde(default = "default_headroom")]
    pub min_worker_headroom_tokens: usize,
    /// Generic errors a worker may make before it is benched for a while (default 2).
    #[serde(default = "default_strikes")]
    pub failures_before_bench: u32,
    /// Seconds a quiet worker is given before it counts as stalled (default 600).
    #[serde(default = "default_stall")]
    pub stall_timeout_secs: u64,
    /// Attempts per unit of work, the first included, before the orchestrator gives up (default 3).
    #[serde(default = "default_attempts")]
    pub max_attempts: u32,
}
fn default_attempts() -> u32 {
    3
}
fn default_fraction() -> f64 {
    0.6
}
fn default_headroom() -> usize {
    8_000
}
fn default_strikes() -> u32 {
    2
}
fn default_stall() -> u64 {
    600
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileProblem {
    NoWorkers,
    /// a worker the mode excludes is listed (reported, not fatal)
    Excluded {
        route: String,
        why: String,
    },
    /// a worker with no known context window is never selected
    UnknownWindow {
        route: String,
    },
    /// rule 2: a task kind with fewer than two candidates has nothing to swap to
    SingleHomed {
        kind: TaskKind,
        route: String,
    },
    /// marked free but the route does not target a free endpoint
    FreeFlagWithoutFreeRoute {
        route: String,
    },
}

impl Profile {
    pub fn new(name: &str, mode: Mode, workers: Vec<Worker>) -> Profile {
        Profile {
            name: name.into(),
            mode,
            workers,
            brief_context_fraction: default_fraction(),
            min_worker_headroom_tokens: default_headroom(),
            failures_before_bench: default_strikes(),
            stall_timeout_secs: default_stall(),
            max_attempts: default_attempts(),
        }
    }
    /// The workers this profile's mode admits.
    pub fn admitted(&self) -> Vec<&Worker> {
        self.workers
            .iter()
            .filter(|w| match &self.mode {
                Mode::FreeOnly => w.free && w.route_says_free(),
                Mode::ProviderOnly(p) => &w.provider == p,
                Mode::PaidOnly => !w.free,
                Mode::Mixed => true,
            })
            .collect()
    }
    /// Everything a person should know before trusting the profile (rules 2, 6, 8).
    pub fn problems(&self) -> Vec<ProfileProblem> {
        let mut out = vec![];
        if self.workers.is_empty() {
            out.push(ProfileProblem::NoWorkers);
            return out;
        }
        for w in &self.workers {
            if w.free && !w.route_says_free() {
                out.push(ProfileProblem::FreeFlagWithoutFreeRoute {
                    route: w.route.clone(),
                });
            }
            if w.context_tokens.is_none() {
                out.push(ProfileProblem::UnknownWindow {
                    route: w.route.clone(),
                });
            }
            let admitted = self.admitted();
            if !admitted.iter().any(|a| a.route == w.route) {
                let why = match &self.mode {
                    Mode::FreeOnly => "not free",
                    Mode::ProviderOnly(p) => {
                        let _ = p;
                        "another provider"
                    }
                    Mode::PaidOnly => "free",
                    Mode::Mixed => "",
                };
                if !why.is_empty() {
                    out.push(ProfileProblem::Excluded {
                        route: w.route.clone(),
                        why: why.into(),
                    });
                }
            }
        }
        let admitted = self.admitted();
        for kind in TaskKind::ALL {
            let cands: Vec<&&Worker> = admitted
                .iter()
                .filter(|w| w.context_tokens.is_some() && w.covers(kind))
                .collect();
            if cands.len() == 1 {
                out.push(ProfileProblem::SingleHomed {
                    kind,
                    route: cands[0].route.clone(),
                });
            }
        }
        out
    }
    /// The largest brief (in tokens) a worker may be handed (rule 6).
    pub fn brief_budget(&self, w: &Worker) -> Option<usize> {
        let window = w.context_tokens?;
        let by_fraction = (window as f64 * self.brief_context_fraction) as usize;
        let by_headroom = window.saturating_sub(self.min_worker_headroom_tokens);
        Some(by_fraction.min(by_headroom))
    }
}

/// A conservative built-in profile for before the user configured anything: free, well-known routes. It is a
/// placeholder; once a profile is configured it is never used (rule 7).
pub fn starter_profile() -> Profile {
    Profile::new(
        "starter-free",
        Mode::FreeOnly,
        vec![
            Worker::new("openrouter:nvidia/nemotron-3-ultra-550b-a55b:free")
                .with(
                    1_000_000,
                    &[
                        TaskKind::Debug,
                        TaskKind::Review,
                        TaskKind::Implement,
                        TaskKind::General,
                        TaskKind::Write,
                    ],
                    true,
                )
                .rank(10),
            Worker::new("openrouter:nvidia/nemotron-3.5-lightning:free")
                .with(
                    1_000_000,
                    &[
                        TaskKind::Summarize,
                        TaskKind::Research,
                        TaskKind::Verify,
                        TaskKind::General,
                        TaskKind::Write,
                    ],
                    true,
                )
                .rank(20),
            Worker::new("openrouter:z-ai/glm-5.2:free")
                .with(
                    256_000,
                    &[TaskKind::Implement, TaskKind::Debug, TaskKind::General],
                    true,
                )
                .rank(30),
            Worker::new("openrouter:google/gemma-4-31b-it:free")
                .with(
                    262_000,
                    &[
                        TaskKind::Write,
                        TaskKind::Research,
                        TaskKind::Review,
                        TaskKind::General,
                    ],
                    true,
                )
                .rank(40),
            Worker::new("openrouter:google/gemma-4-26b-a4b-it:free")
                .with(
                    262_000,
                    &[
                        TaskKind::Verify,
                        TaskKind::Summarize,
                        TaskKind::Research,
                        TaskKind::Write,
                    ],
                    true,
                )
                .rank(50),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_admit_the_right_workers() {
        let free = Worker::new("openrouter:x/y:free").with(100_000, &[TaskKind::General], true);
        let paid = Worker::new("openrouter:x/y").with(100_000, &[TaskKind::General], false);
        let local = Worker::new("ollama:qwen3-coder:30b").with(
            64_000,
            &[TaskKind::Implement, TaskKind::General],
            true,
        );
        let claude = Worker::new("claude-api:claude-haiku-5-5").with(
            200_000,
            &[TaskKind::Review, TaskKind::General],
            false,
        );
        let all = vec![free.clone(), paid.clone(), local.clone(), claude.clone()];
        let routes = |p: &Profile| {
            p.admitted()
                .iter()
                .map(|w| w.route.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            routes(&Profile::new("f", Mode::FreeOnly, all.clone())),
            ["openrouter:x/y:free", "ollama:qwen3-coder:30b"]
        );
        assert_eq!(
            routes(&Profile::new("p", Mode::PaidOnly, all.clone())),
            ["openrouter:x/y", "claude-api:claude-haiku-5-5"]
        );
        assert_eq!(
            routes(&Profile::new(
                "o",
                Mode::ProviderOnly("ollama".into()),
                all.clone()
            )),
            ["ollama:qwen3-coder:30b"]
        );
        assert_eq!(
            routes(&Profile::new("m", Mode::Mixed, all.clone())).len(),
            4
        );
    }

    #[test]
    fn problems_name_single_homed_kinds_unknown_windows_and_false_free_flags() {
        let p = Profile::new(
            "t",
            Mode::Mixed,
            vec![
                Worker::new("openrouter:a/b").with(
                    100_000,
                    &[TaskKind::Implement, TaskKind::General],
                    true,
                ), // free flag, paid route
                Worker::new("openrouter:c/d:free").with(100_000, &[TaskKind::General], true),
                Worker {
                    context_tokens: None,
                    ..Worker::new("ollama:nowindow").with(0, &[TaskKind::Review], true)
                },
            ],
        );
        let probs = p.problems();
        assert!(probs.contains(&ProfileProblem::FreeFlagWithoutFreeRoute {
            route: "openrouter:a/b".into()
        }));
        assert!(probs.contains(&ProfileProblem::UnknownWindow {
            route: "ollama:nowindow".into()
        }));
        assert!(probs.contains(&ProfileProblem::SingleHomed {
            kind: TaskKind::Implement,
            route: "openrouter:a/b".into()
        }));
        assert!(
            !probs.iter().any(|x| matches!(
                x,
                ProfileProblem::SingleHomed {
                    kind: TaskKind::General,
                    ..
                }
            )),
            "general has two candidates"
        );
        assert!(Profile::new("e", Mode::Mixed, vec![])
            .problems()
            .contains(&ProfileProblem::NoWorkers));
        assert!(
            starter_profile().problems().iter().all(|x| !matches!(
                x,
                ProfileProblem::SingleHomed { .. } | ProfileProblem::UnknownWindow { .. }
            )),
            "the starter profile is two-homed everywhere"
        );
    }

    #[test]
    fn the_brief_budget_takes_the_stricter_bound() {
        let p = Profile::new("t", Mode::Mixed, vec![]);
        let big = Worker::new("x:big").with(1_000_000, &[], true);
        let small = Worker::new("x:small").with(10_000, &[], true);
        assert_eq!(p.brief_budget(&big), Some(600_000)); // fraction wins
        assert_eq!(p.brief_budget(&small), Some(2_000)); // headroom wins (10,000 - 8,000)
        assert_eq!(p.brief_budget(&Worker::new("x:none")), None);
    }

    #[test]
    fn profiles_round_trip_through_json() {
        let p = starter_profile();
        let s = serde_json::to_string(&p).unwrap();
        let back: Profile = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
        assert!(s.contains("\"mode\":\"free_only\""));
    }
}
