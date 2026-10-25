//! Picking a worker for a brief: specialists before generalists, free before paid, then load (spreading), then the
//! user's rank, then proven successes. Skips benched workers, workers that refused this kind, workers whose trust
//! forbids the kind, and workers whose window cannot hold the brief. Returns ordered fallbacks so the dispatcher can
//! swap without re-running policy. Unroutable is an error, never a fall-back to the orchestrator's model (rule 5).
use crate::health::Health;
use crate::profile::{Profile, TaskKind, Trust, Worker};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub kind: TaskKind,
    /// the chosen worker's route, then the fallbacks in order
    pub routes: Vec<String>,
    /// why each admitted worker was skipped (route, reason)
    pub skipped: Vec<(String, String)>,
}

impl Selection {
    pub fn chosen(&self) -> Option<&str> { self.routes.first().map(String::as_str) }
    pub fn fallbacks(&self) -> &[String] { if self.routes.is_empty() { &[] } else { &self.routes[1..] } }
}

/// The task kind of a brief from its words, when the orchestrator did not say (`auto`): a cheap heuristic that only
/// has to be roughly right (the orchestrator can always say `auto:<kind>`).
pub fn infer_task_kind(brief: &str) -> TaskKind {
    let t = brief.to_ascii_lowercase();
    let score = |words: &[&str]| words.iter().filter(|w| t.contains(*w)).count();
    let table = [
        (TaskKind::Debug, score(&["bug", "fix", "failing", "error", "crash", "panic", "debug", "regression"])),
        (TaskKind::Verify, score(&["verify", "test", "check that", "confirm", "run the tests", "assert", "validate"])),
        (TaskKind::Review, score(&["review", "audit", "look over", "critique", "assess", "security"])),
        (TaskKind::Research, score(&["research", "find out", "look up", "investigate", "compare", "survey", "docs for", "documentation"])),
        (TaskKind::Summarize, score(&["summarize", "summary", "tl;dr", "condense", "recap", "digest"])),
        (TaskKind::Write, score(&["write a doc", "document", "readme", "changelog", "blog", "release notes", "explain in prose"])),
        (TaskKind::Implement, score(&["implement", "add a", "build", "create", "refactor", "write code", "function", "struct", "feature"])),
    ];
    table.iter().max_by_key(|(_, s)| *s).filter(|(_, s)| *s > 0).map(|(k, _)| *k).unwrap_or(TaskKind::General)
}

/// `auto` / `auto:<kind>` / a concrete route: what the orchestrator asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask { Auto(Option<TaskKind>), Concrete(String) }
pub fn parse_ask(model: &str) -> Ask {
    let m = model.trim();
    if m.is_empty() || m.eq_ignore_ascii_case("auto") { return Ask::Auto(None); }
    if let Some(k) = m.strip_prefix("auto:") { return Ask::Auto(TaskKind::parse(k)); }
    Ask::Concrete(m.to_string())
}

pub fn select(profile: &Profile, health: &Health, kind: TaskKind, brief_tokens: usize, now: u64) -> Selection {
    let mut skipped = vec![];
    let mut cands: Vec<&Worker> = vec![];
    for w in profile.admitted() {
        if health.benched(&w.route, now) { skipped.push((w.route.clone(), "benched".into())); continue; }
        if health.refuses(&w.route, kind) { skipped.push((w.route.clone(), format!("refused {} before", kind.as_str()))); continue; }
        if kind.mutates() && w.trust == Trust::ReadOnly { skipped.push((w.route.clone(), "read-only trust: no repo-mutating work".into())); continue; }
        match profile.brief_budget(w) {
            None => { skipped.push((w.route.clone(), "context window unknown".into())); continue; }
            Some(b) if b < brief_tokens => { skipped.push((w.route.clone(), format!("brief {brief_tokens} tokens > budget {b}"))); continue; }
            _ => {}
        }
        if w.good_at.is_empty() { skipped.push((w.route.clone(), "no task kinds listed".into())); continue; }
        cands.push(w);
    }
    // specialists (good at this kind) before generalists (good at general only)
    let specialist = |w: &Worker| w.good_at.contains(&kind);
    let generalist = |w: &Worker| w.good_at.contains(&TaskKind::General);
    cands.retain(|w| specialist(w) || generalist(w));
    cands.sort_by(|a, b| {
        specialist(b).cmp(&specialist(a))
            .then(b.free.cmp(&a.free))
            .then(health.load(&a.route).cmp(&health.load(&b.route)))
            .then(a.rank.cmp(&b.rank))
            .then(health.successes(&b.route).cmp(&health.successes(&a.route)))
            .then(a.route.cmp(&b.route))
    });
    Selection { kind, routes: cands.iter().map(|w| w.route.clone()).collect(), skipped }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::Outcome;
    use crate::profile::Mode;

    fn profile() -> Profile {
        Profile::new("t", Mode::Mixed, vec![
            Worker::new("openrouter:spec/a:free").with(100_000, &[TaskKind::Implement, TaskKind::General], true).rank(50),
            Worker::new("openrouter:spec/b:free").with(100_000, &[TaskKind::Implement], true).rank(10),
            Worker::new("openrouter:gen/c:free").with(100_000, &[TaskKind::General], true).rank(1),
            Worker::new("claude-api:claude-haiku-5-5").with(200_000, &[TaskKind::Implement, TaskKind::Review, TaskKind::General], false).rank(5),
            Worker::new("openrouter:small/d:free").with(8_500, &[TaskKind::Implement], true), // budget 500 < the 1,000-token brief
            Worker::new("ollama:ro").with(64_000, &[TaskKind::Implement, TaskKind::General], true).trust(Trust::ReadOnly),
        ])
    }

    #[test]
    fn specialists_then_free_then_load_then_rank_then_successes() {
        let p = profile(); let mut h = Health::new();
        let s = select(&p, &h, TaskKind::Implement, 1_000, 0);
        // specialists first: b (rank 10), a (rank 50), haiku (paid specialist); generalist c last; small/d and ro skipped
        assert_eq!(s.routes, ["openrouter:spec/b:free", "openrouter:spec/a:free", "claude-api:claude-haiku-5-5", "openrouter:gen/c:free"]);
        assert!(s.skipped.iter().any(|(r, why)| r == "openrouter:small/d:free" && why.contains("budget")));
        assert!(s.skipped.iter().any(|(r, why)| r == "ollama:ro" && why.contains("read-only")));
        // load before rank: b busy twice → a first
        h.started("openrouter:spec/b:free"); h.started("openrouter:spec/b:free");
        assert_eq!(select(&p, &h, TaskKind::Implement, 1_000, 0).chosen(), Some("openrouter:spec/a:free"));
        // a bench removes it; a refusal of this kind removes it for good
        h.record("openrouter:spec/a:free", TaskKind::Implement, Outcome::Unavailable, 10, 2, 600, "403");
        let s2 = select(&p, &h, TaskKind::Implement, 1_000, 11);
        assert_eq!(s2.chosen(), Some("openrouter:spec/b:free"));
        assert!(s2.skipped.iter().any(|(r, w)| r == "openrouter:spec/a:free" && w == "benched"));
    }

    #[test]
    fn read_only_trust_may_do_review_and_an_oversized_brief_is_unroutable() {
        let p = profile(); let h = Health::new();
        let s = select(&p, &h, TaskKind::Review, 1_000, 0);
        assert!(s.routes.contains(&"ollama:ro".to_string()) && s.chosen() == Some("claude-api:claude-haiku-5-5"), "{:?}", s.routes); // the only specialist for review
        let big = select(&p, &h, TaskKind::Implement, 500_000, 0);
        assert!(big.routes.is_empty(), "nothing can hold it: unroutable, not the orchestrator's model");
        assert_eq!(big.skipped.len(), 6);
    }

    #[test]
    fn asks_and_kinds_parse_and_infer() {
        assert_eq!(parse_ask("auto"), Ask::Auto(None));
        assert_eq!(parse_ask(""), Ask::Auto(None));
        assert_eq!(parse_ask("auto:debug"), Ask::Auto(Some(TaskKind::Debug)));
        assert_eq!(parse_ask("auto:nonsense"), Ask::Auto(None));
        assert_eq!(parse_ask("claude-api:claude-haiku-5-5"), Ask::Concrete("claude-api:claude-haiku-5-5".into()));
        assert_eq!(infer_task_kind("Fix the failing test in parser.rs, it panics on empty input"), TaskKind::Debug);
        assert_eq!(infer_task_kind("Research the docs for tree-sitter queries and compare two approaches"), TaskKind::Research);
        assert_eq!(infer_task_kind("Implement a function that parses TOML into the Profile struct"), TaskKind::Implement);
        assert_eq!(infer_task_kind("hello there"), TaskKind::General);
    }
}
