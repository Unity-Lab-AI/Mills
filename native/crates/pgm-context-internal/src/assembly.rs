//! Cache-friendly prompt assembly: a prompt is a set of segments, each with a stability class. The planner puts the
//! most stable first (in a fixed order, never insertion order), hashes each segment, places cache breakpoints the
//! way the provider wants them, and lints: volatile text ("3 min ago", a timestamp, a random id) inside a Static or
//! Session segment invalidates the cache for everything after it, so the lint names the segment to move to Turn.
use crate::short_hash;
use crate::tokens::Counter;
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// The same for every session on this machine: tool definitions, the base prompt, the laws.
    Static,
    /// The same for the whole session: the project overlay, memory, the roster.
    Session,
    /// This turn only: the user's message, notes that change ("latest check 3 min ago"), attachments.
    Turn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment { pub id: String, pub class: Class, pub text: String }
impl Segment {
    pub fn new(id: &str, class: Class, text: impl Into<String>) -> Segment { Segment { id: id.into(), class, text: text.into() } }
}

/// What a provider's cache wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// Explicit breakpoints the request may carry (Anthropic: 4; OpenAI / Gemini implicit: 0).
    pub explicit_breakpoints: usize,
    /// Below this many tokens a prefix is not cached at all.
    pub min_cacheable: usize,
    pub ttl_secs: u64,
}
impl Caps {
    pub fn anthropic() -> Caps { Caps { explicit_breakpoints: 4, min_cacheable: 1024, ttl_secs: 300 } }
    /// Haiku-class models need a bigger prefix before Anthropic caches it.
    pub fn anthropic_haiku() -> Caps { Caps { explicit_breakpoints: 4, min_cacheable: 2048, ttl_secs: 300 } }
    pub fn openai() -> Caps { Caps { explicit_breakpoints: 0, min_cacheable: 1024, ttl_secs: 300 } }
    pub fn gemini() -> Caps { Caps { explicit_breakpoints: 0, min_cacheable: 1024, ttl_secs: 3600 } }
    /// A local server with KV prefix caching (vLLM, llama.cpp): no markers, any size, lives while the process does.
    pub fn local() -> Caps { Caps { explicit_breakpoints: 0, min_cacheable: 1, ttl_secs: 0 } }
    pub fn for_model(model: &str) -> Caps {
        let m = model.to_ascii_lowercase();
        if m.contains("claude") { if m.contains("haiku") { Caps::anthropic_haiku() } else { Caps::anthropic() } }
        else if m.starts_with("gpt") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") { Caps::openai() }
        else if m.contains("gemini") { Caps::gemini() }
        else { Caps::local() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed { pub id: String, pub class: Class, pub text: String, pub hash: String, pub tokens: usize, pub breakpoint_after: bool }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintHit { pub segment: String, pub class: Class, pub pattern: &'static str, pub snippet: String }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub ordered: Vec<Placed>,
    pub lint: Vec<LintHit>,
    /// Tokens in Static + Session segments (what can come from the cache after the first turn).
    pub stable_tokens: usize,
    /// Tokens in Turn segments (paid every time, by design).
    pub turn_tokens: usize,
    /// Hash of the stable prefix: the same string turn after turn means the cache can hit.
    pub prefix_hash: String,
}

impl Plan {
    /// The prompt text: segments in planned order, separated by blank lines.
    pub fn render(&self) -> String { self.ordered.iter().map(|p| p.text.trim_end()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n") }
    /// The ids of segments with a breakpoint after them, in order.
    pub fn breakpoints(&self) -> Vec<String> { self.ordered.iter().filter(|p| p.breakpoint_after).map(|p| p.id.clone()).collect() }
}

struct Pattern { name: &'static str, re: Regex, classes: &'static [Class] }
fn patterns() -> &'static [Pattern] {
    static P: OnceLock<Vec<Pattern>> = OnceLock::new();
    P.get_or_init(|| vec![
        Pattern { name: "relative time (\"N min ago\")", re: Regex::new(r"(?i)\b\d+\s*(min|mins|minute|minutes|sec|secs|second|seconds|h|hr|hrs|hour|hours|day|days)\s+ago\b").unwrap(), classes: &[Class::Static, Class::Session] },
        Pattern { name: "timestamp", re: Regex::new(r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2})?").unwrap(), classes: &[Class::Static, Class::Session] },
        Pattern { name: "clock time (\"at 14:05\")", re: Regex::new(r"(?i)\b(at|time|now)\s+\d{1,2}:\d{2}(:\d{2})?\b").unwrap(), classes: &[Class::Static] },
        Pattern { name: "random id (16+ hex chars)", re: Regex::new(r"\b[0-9a-f]{16,}\b").unwrap(), classes: &[Class::Static, Class::Session] },
        Pattern { name: "live counter (\"N online\", \"N people here\")", re: Regex::new(r"(?i)\b\d+\s+(people\s+)?(online|here|connected|waiting)\b").unwrap(), classes: &[Class::Static, Class::Session] },
    ])
}

/// Lints one text as if it were in `class`: every volatile pattern that would bust the cache.
pub fn lint(id: &str, class: Class, text: &str) -> Vec<LintHit> {
    patterns().iter().filter(|p| p.classes.contains(&class)).filter_map(|p| p.re.find(text).map(|m| {
        let s = m.start().saturating_sub(20); let e = (m.end() + 20).min(text.len());
        let (s, e) = (floor_char(text, s), ceil_char(text, e));
        LintHit { segment: id.into(), class, pattern: p.name, snippet: text[s..e].replace('\n', " ") }
    })).collect()
}
fn floor_char(t: &str, mut i: usize) -> usize { while i > 0 && !t.is_char_boundary(i) { i -= 1; } i }
fn ceil_char(t: &str, mut i: usize) -> usize { while i < t.len() && !t.is_char_boundary(i) { i += 1; } i }

/// Orders, hashes, counts and places breakpoints. Segments keep their text; only the order and the markers are the
/// planner's. Order: Static, Session, Turn; within a class by id (stable across turns whatever the caller's order).
pub fn plan(segments: &[Segment], caps: Caps, counter: &dyn Counter) -> Plan {
    let mut segs: Vec<&Segment> = segments.iter().collect();
    segs.sort_by(|a, b| a.class.cmp(&b.class).then_with(|| a.id.cmp(&b.id)));
    let mut ordered: Vec<Placed> = segs.iter().map(|s| Placed { id: s.id.clone(), class: s.class, text: s.text.clone(), hash: short_hash(&s.text), tokens: counter.count(&s.text), breakpoint_after: false }).collect();
    let lint: Vec<LintHit> = segs.iter().flat_map(|s| lint(&s.id, s.class, &s.text)).collect();
    let stable_tokens: usize = ordered.iter().filter(|p| p.class != Class::Turn).map(|p| p.tokens).sum();
    let turn_tokens: usize = ordered.iter().filter(|p| p.class == Class::Turn).map(|p| p.tokens).sum();
    // breakpoints: after the last Static segment when the Static block is big enough, after the last Session segment
    // when the Session block is big enough on its own (a breakpoint only pays when the block before it is ≥ minimum)
    let mut placed = 0;
    if caps.explicit_breakpoints > 0 {
        let static_tokens: usize = ordered.iter().filter(|p| p.class == Class::Static).map(|p| p.tokens).sum();
        let session_tokens: usize = ordered.iter().filter(|p| p.class == Class::Session).map(|p| p.tokens).sum();
        if static_tokens >= caps.min_cacheable { if let Some(i) = ordered.iter().rposition(|p| p.class == Class::Static) { ordered[i].breakpoint_after = true; placed += 1; } }
        if placed < caps.explicit_breakpoints && session_tokens >= caps.min_cacheable { if let Some(i) = ordered.iter().rposition(|p| p.class == Class::Session) { ordered[i].breakpoint_after = true; } }
    }
    let prefix: String = ordered.iter().filter(|p| p.class != Class::Turn).map(|p| p.hash.as_str()).collect::<Vec<_>>().join("|");
    Plan { prefix_hash: short_hash(&prefix), ordered, lint, stable_tokens, turn_tokens }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Estimator;

    fn big(n: usize) -> String { "word ".repeat(n) }

    #[test]
    fn order_is_by_class_then_id_whatever_the_caller_did() {
        let c = Estimator::generic();
        let segs = vec![Segment::new("user", Class::Turn, "do it"), Segment::new("laws", Class::Static, "LAWS"), Segment::new("project", Class::Session, "proj"), Segment::new("base", Class::Static, "BASE")];
        let p = plan(&segs, Caps::anthropic(), &c);
        let ids: Vec<&str> = p.ordered.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["base", "laws", "project", "user"]);
        assert_eq!(p.render(), "BASE\n\nLAWS\n\nproj\n\ndo it");
        let again = plan(&[segs[2].clone(), segs[3].clone(), segs[1].clone(), Segment::new("user", Class::Turn, "something else")], Caps::anthropic(), &c);
        assert_eq!(p.prefix_hash, again.prefix_hash, "the stable prefix hash ignores the turn and the caller's order");
    }

    #[test]
    fn breakpoints_go_after_big_enough_stable_blocks() {
        let c = Estimator::generic();
        let segs = vec![Segment::new("a-tools", Class::Static, big(3000)), Segment::new("b-laws", Class::Static, "short"), Segment::new("project", Class::Session, big(1500)), Segment::new("user", Class::Turn, "hi")];
        let p = plan(&segs, Caps::anthropic(), &c);
        assert_eq!(p.breakpoints(), ["b-laws", "project"]);
        assert!(p.stable_tokens > 4000 && p.turn_tokens < 5);
        let small = plan(&[Segment::new("s", Class::Static, "tiny"), Segment::new("u", Class::Turn, "hi")], Caps::anthropic(), &c);
        assert!(small.breakpoints().is_empty(), "a tiny prefix gets no breakpoint: it would not be cached anyway");
        let haiku = plan(&segs, Caps::anthropic_haiku(), &c);
        assert_eq!(haiku.breakpoints(), ["b-laws"], "the session block is under Haiku's 2048 minimum");
        let oai = plan(&segs, Caps::openai(), &c);
        assert!(oai.breakpoints().is_empty(), "OpenAI caches implicitly: no markers");
    }

    #[test]
    fn the_lint_catches_volatile_text_in_stable_segments_only() {
        let c = Estimator::generic();
        let fresh = "Worktree law: check it. Latest background check (3 min ago): level with origin/develop · FRESH";
        let p = plan(&[Segment::new("laws", Class::Static, fresh), Segment::new("user", Class::Turn, fresh)], Caps::anthropic(), &c);
        assert_eq!(p.lint.len(), 1);
        assert_eq!(p.lint[0].segment, "laws"); assert!(p.lint[0].pattern.contains("ago")); assert!(p.lint[0].snippet.contains("3 min ago"));
        let hits = lint("x", Class::Session, "run id a1b2c3d4e5f6a7b8c9d0 at 2026-10-23 00:15 with 4 people online");
        let names: Vec<&str> = hits.iter().map(|h| h.pattern).collect();
        assert!(names.iter().any(|n| n.contains("timestamp")) && names.iter().any(|n| n.contains("random id")) && names.iter().any(|n| n.contains("counter")));
        assert!(lint("u", Class::Turn, "3 min ago 2026-10-23 00:15").is_empty());
        assert!(lint("s", Class::Static, "the hex 0123456789abcdef0123 is a random id").len() == 1);
        assert!(lint("s", Class::Static, "sha short 0123456789ab stays").is_empty(), "12 hex chars are not flagged");
    }

    #[test]
    fn caps_follow_the_model() {
        assert_eq!(Caps::for_model("claude-haiku-5-5").min_cacheable, 2048);
        assert_eq!(Caps::for_model("claude-sonnet-5-5").min_cacheable, 1024);
        assert_eq!(Caps::for_model("gpt-5.5").explicit_breakpoints, 0);
        assert_eq!(Caps::for_model("gemini-3-pro").ttl_secs, 3600);
        assert_eq!(Caps::for_model("qwen3-coder").min_cacheable, 1);
    }
}
