//! The offline cache simulator (TOKENS.md §7): replay a recorded sequence of requests through a provider's caching
//! rules and report what would have been cached vs paid, with and without the shapers and the prompt planner. That is
//! the before/after number without spending a subscription on it.
//!
//! The model of a request: an ordered list of UNITS, first the prefix segments in the order [`assembly::plan`] puts
//! them (Static, Session, Turn; by id inside a class), then the messages. Caches work on prefixes, and a changed unit
//! changes everything after it, so comparison is by unit hash, never by character.
//!
//! Provider rules (see [`assembly::Caps`]):
//! - Explicit breakpoints (Anthropic, `explicit_breakpoints > 0`): a prefix is stored at each breakpoint (the planner's
//!   after the Static / Session blocks, then the newest messages, two at most, within the 4 allowed). A request reads the
//!   longest stored prefix that one of its breakpoints reaches, looking back up to 20 units from each breakpoint (the
//!   provider's lookback window). A prefix under `min_cacheable` tokens is neither stored nor read. The part between
//!   the read and the last breakpoint is WRITTEN (billed at the write rate); the rest after it is paid in full.
//! - Implicit (OpenAI, Gemini; no markers): the common unit prefix with the PREVIOUS request counts as cached when it is
//!   at least `min_cacheable` tokens. Local servers (`min_cacheable == 1`) are the same rule, so the whole common prefix
//!   counts. Everything after the first changed unit is paid.
//!
//! `paid_tokens` is `input - cached` (written tokens are part of it, at their own rate in the cost estimate). No clock:
//! the TTL is not simulated, a stored prefix stays stored.
use crate::assembly::{self, Caps, Class, Segment};
use crate::shape::{Limits, Shaper};
use crate::short_hash;
use crate::tokens::{self, Counter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// How far back from a breakpoint the provider looks for an earlier stored prefix, in content blocks (units here).
const LOOKBACK: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimMessage {
    pub id: String,
    /// "user", "assistant", "tool" ...: tool outputs ("tool", "tool_result") are what `shape` acts on.
    pub role: String,
    pub text: String,
    /// The recorded token count, if the provider reported one; otherwise the counter counts `text`.
    pub tokens: Option<u64>,
}

impl SimMessage {
    pub fn new(id: &str, role: &str, text: &str) -> SimMessage {
        SimMessage {
            id: id.into(),
            role: role.into(),
            text: text.into(),
            tokens: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimRequest {
    pub turn: u32,
    /// Tools, the static system prompt, the dynamic system text, each with its stability class.
    pub prefix: Vec<Segment>,
    pub messages: Vec<SimMessage>,
}

pub struct SimOptions {
    pub caps: Caps,
    /// Run tool messages through the shapers (as a [`Shaper::dry`], lossy passes included, a stash handle line added).
    /// Each message is shaped once, when it first appears, and keeps that form: it is what entered the context.
    pub shape: bool,
    /// Move Turn-class prefix segments to the end of the last message (the pattern Claude Code uses for its reminders),
    /// so volatile text no longer sits ahead of the transcript. Without messages they stay in the prefix.
    pub dynamic_in_turn: bool,
    pub counter: Box<dyn Counter>,
    /// The input price per million tokens, if a cost estimate is wanted.
    pub price_per_mtok: Option<f64>,
    /// A cache read costs this share of the input price (Anthropic 0.10, OpenAI 0.50, local 1.0: no discount).
    pub read_factor: f64,
    /// A cache write costs this share (Anthropic 1.25, the others 1.0).
    pub write_factor: f64,
    pub shape_limits: Limits,
}

impl SimOptions {
    /// Options for `caps` and `counter`, with the price factors that provider family has. Shaping and the dynamic
    /// move are off: they are what a "before vs after" comparison turns on.
    pub fn new(caps: Caps, counter: Box<dyn Counter>) -> SimOptions {
        let (read_factor, write_factor) = if caps.explicit_breakpoints > 0 {
            (0.10, 1.25)
        } else if caps.min_cacheable > 1 {
            (0.50, 1.0)
        } else {
            (1.0, 1.0)
        };
        SimOptions {
            caps,
            shape: false,
            dynamic_in_turn: false,
            counter,
            price_per_mtok: None,
            read_factor,
            write_factor,
            shape_limits: Limits::default(),
        }
    }
    pub fn for_model(model: &str) -> SimOptions {
        SimOptions::new(Caps::for_model(model), tokens::for_model(model))
    }
    pub fn with_shape(mut self, on: bool) -> SimOptions {
        self.shape = on;
        self
    }
    pub fn with_dynamic_in_turn(mut self, on: bool) -> SimOptions {
        self.dynamic_in_turn = on;
        self
    }
    pub fn with_price(mut self, per_mtok: f64) -> SimOptions {
        self.price_per_mtok = Some(per_mtok);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnSim {
    pub turn: u32,
    pub input_tokens: u64,
    /// Read from the cache.
    pub cached_tokens: u64,
    /// Newly stored in the cache by this request (billed at the write rate on explicit-breakpoint providers).
    pub written_tokens: u64,
    /// `input - cached`: everything not read from the cache (the written tokens are part of it).
    pub paid_tokens: u64,
    pub hit_ratio: f32,
    /// Hash of the stable (Static + Session) prefix.
    pub prefix_hash: String,
    /// The stable prefix differs from the previous request's.
    pub prefix_changed: bool,
    /// Why the cache missed, when the simulator can tell (names the segment or message).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Totals {
    pub input: u64,
    pub cached: u64,
    pub written: u64,
    pub paid: u64,
    pub hit_ratio: f64,
    pub cost_estimate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimReport {
    /// What was simulated: caps and counter, one line.
    pub setup: String,
    pub per_turn: Vec<TurnSim>,
    pub totals: Totals,
    /// Set on the "after" report of [`compare`]: (totals before, totals after).
    pub before_vs_after: Option<Box<(Totals, Totals)>>,
}

struct Unit {
    label: String,
    class: Option<Class>,
    hash: String,
    tokens: u64,
    breakpoint: bool,
}

struct Prev {
    hashes: Vec<String>,
    labels: Vec<String>,
    stable: Vec<(String, String)>,
    n_messages: usize,
}

fn is_tool(role: &str) -> bool {
    role == "tool" || role == "tool_result" || role == "function"
}

/// Replays `requests` in order through the caching rules of `opts.caps`.
pub fn simulate(requests: &[SimRequest], opts: &SimOptions) -> SimReport {
    let mut shaper = Shaper::dry(opts.shape_limits);
    let mut shaped: HashMap<String, (String, Option<u64>)> = HashMap::new();
    let mut stored: HashSet<String> = HashSet::new();
    let mut prev: Option<Prev> = None;
    let mut per_turn = vec![];
    for req in requests {
        let plan = assembly::plan(&req.prefix, opts.caps, opts.counter.as_ref());
        let mut units: Vec<Unit> = vec![];
        let mut moved = String::new();
        let mut moved_any = false;
        for p in &plan.ordered {
            if opts.dynamic_in_turn && p.class == Class::Turn && !req.messages.is_empty() {
                if moved_any {
                    moved.push_str("\n\n");
                }
                moved.push_str(p.text.trim_end());
                moved_any = true;
                continue;
            }
            units.push(Unit {
                label: format!("segment:{}", p.id),
                class: Some(p.class),
                hash: p.hash.clone(),
                tokens: p.tokens as u64,
                breakpoint: p.breakpoint_after,
            });
        }
        let n_prefix = units.len();
        let last = req.messages.len().saturating_sub(1);
        for (i, m) in req.messages.iter().enumerate() {
            let (mut text, mut tokens) = if opts.shape && is_tool(&m.role) {
                let e = shaped.entry(m.id.clone()).or_insert_with(|| {
                    let s = shaper.shape_tool("bash", None, &m.text, req.turn as usize);
                    let t = if s.text == m.text { m.tokens } else { None };
                    (s.text, t)
                });
                e.clone()
            } else {
                (m.text.clone(), m.tokens)
            };
            if i == last && moved_any {
                text = format!("{text}\n\n{moved}");
                tokens = None;
            }
            units.push(Unit {
                label: format!("message:{}", m.id),
                class: None,
                hash: short_hash(&format!("{}\u{0}{}", m.role, text)),
                tokens: tokens.unwrap_or_else(|| opts.counter.count(&text) as u64),
                breakpoint: false,
            });
        }
        // explicit providers: what is left of the breakpoint budget goes on the newest messages
        if opts.caps.explicit_breakpoints > 0 {
            let used = units.iter().filter(|u| u.breakpoint).count();
            let spare = opts.caps.explicit_breakpoints.saturating_sub(used).min(2);
            for u in units[n_prefix..].iter_mut().rev().take(spare) {
                u.breakpoint = true;
            }
        }
        let mut cum_tokens = Vec::with_capacity(units.len());
        let mut cum_hash: Vec<String> = Vec::with_capacity(units.len());
        let (mut t, mut h) = (0u64, String::new());
        for u in &units {
            t += u.tokens;
            h = short_hash(&format!("{h}|{}", u.hash));
            cum_tokens.push(t);
            cum_hash.push(h.clone());
        }
        let input = t;
        let min = opts.caps.min_cacheable as u64;
        let (cached, written);
        if opts.caps.explicit_breakpoints > 0 {
            let mut best = 0u64;
            let mut top = 0u64;
            for (b, u) in units.iter().enumerate() {
                if !u.breakpoint || cum_tokens[b] < min {
                    continue;
                }
                top = top.max(cum_tokens[b]);
                for k in (b.saturating_sub(LOOKBACK)..=b).rev() {
                    if cum_tokens[k] >= min && stored.contains(&cum_hash[k]) {
                        best = best.max(cum_tokens[k]);
                        break;
                    }
                }
            }
            for (b, u) in units.iter().enumerate() {
                if u.breakpoint && cum_tokens[b] >= min {
                    stored.insert(cum_hash[b].clone());
                }
            }
            cached = best;
            written = top.saturating_sub(best);
        } else {
            let common = prev.as_ref().map_or(0, |p| {
                (0..units.len().min(p.hashes.len()))
                    .take_while(|&i| units[i].hash == p.hashes[i] && units[i].label == p.labels[i])
                    .count()
            });
            let c = if common > 0 {
                cum_tokens[common - 1]
            } else {
                0
            };
            cached = if c >= min { c } else { 0 };
            written = if input >= min {
                input.saturating_sub(cached)
            } else {
                0
            };
        }
        let stable: Vec<(String, String)> = plan
            .ordered
            .iter()
            .filter(|p| p.class != Class::Turn)
            .map(|p| (p.id.clone(), p.hash.clone()))
            .collect();
        let (mut prefix_changed, mut reason) = (false, None);
        if let Some(p) = &prev {
            let old: HashMap<&str, &str> = p
                .stable
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            let mut why: Vec<String> = vec![];
            for (id, hash) in &stable {
                if old.get(id.as_str()) != Some(&hash.as_str()) {
                    let class = plan
                        .ordered
                        .iter()
                        .find(|x| &x.id == id)
                        .map(|x| x.class)
                        .unwrap_or(Class::Static);
                    let lint: Vec<&str> = plan
                        .lint
                        .iter()
                        .filter(|l| &l.segment == id)
                        .map(|l| l.pattern)
                        .collect();
                    let tail = if lint.is_empty() {
                        String::new()
                    } else {
                        format!(" (volatile text: {})", lint.join(", "))
                    };
                    why.push(format!("{class:?} segment '{id}' changed{tail}"));
                }
            }
            for id in old.keys() {
                if !stable.iter().any(|(s, _)| s == id) {
                    why.push(format!("segment '{id}' disappeared"));
                }
            }
            why.sort();
            if !why.is_empty() {
                prefix_changed = true;
                reason = Some(format!("prefix changed: {}", why.join("; ")));
            } else if let Some(i) = (0..units.len().min(p.hashes.len()))
                .find(|&i| units[i].hash != p.hashes[i] || units[i].label != p.labels[i])
            {
                let u = &units[i];
                if u.class == Some(Class::Turn) && units.len() > n_prefix {
                    reason = Some(format!(
                        "turn-class {} sits ahead of the transcript and changed, so the messages after it miss the cache (dynamic_in_turn moves it)",
                        u.label
                    ));
                } else if u.class.is_none() && i + 1 < p.n_messages + n_prefix.min(p.labels.len()) {
                    reason = Some(format!(
                        "history changed at {} (everything after it is paid)",
                        u.label
                    ));
                }
            }
        }
        per_turn.push(TurnSim {
            turn: req.turn,
            input_tokens: input,
            cached_tokens: cached,
            written_tokens: written,
            paid_tokens: input.saturating_sub(cached),
            hit_ratio: if input > 0 {
                cached as f32 / input as f32
            } else {
                0.0
            },
            prefix_hash: plan.prefix_hash.clone(),
            prefix_changed,
            reason,
        });
        prev = Some(Prev {
            hashes: units.iter().map(|u| u.hash.clone()).collect(),
            labels: units.iter().map(|u| u.label.clone()).collect(),
            stable,
            n_messages: req.messages.len(),
        });
    }
    let mut totals = Totals::default();
    for t in &per_turn {
        totals.input += t.input_tokens;
        totals.cached += t.cached_tokens;
        totals.written += t.written_tokens;
        totals.paid += t.paid_tokens;
    }
    if totals.input > 0 {
        totals.hit_ratio = totals.cached as f64 / totals.input as f64;
    }
    totals.cost_estimate = opts.price_per_mtok.map(|p| {
        let full = totals.paid.saturating_sub(totals.written) as f64;
        (full + totals.written as f64 * opts.write_factor + totals.cached as f64 * opts.read_factor)
            * p
            / 1_000_000.0
    });
    SimReport {
        setup: format!(
            "{} breakpoints, min {} tokens, counter {}{}{}",
            opts.caps.explicit_breakpoints,
            opts.caps.min_cacheable,
            opts.counter.family(),
            if opts.shape { ", shaped" } else { "" },
            if opts.dynamic_in_turn {
                ", volatile text in the last message"
            } else {
                ""
            }
        ),
        per_turn,
        totals,
        before_vs_after: None,
    }
}

/// The same requests under two settings; the second report carries `before_vs_after`.
pub fn compare(
    requests: &[SimRequest],
    opts_before: &SimOptions,
    opts_after: &SimOptions,
) -> (SimReport, SimReport) {
    let before = simulate(requests, opts_before);
    let mut after = simulate(requests, opts_after);
    after.before_vs_after = Some(Box::new((before.totals, after.totals)));
    (before, after)
}

impl SimReport {
    /// Percent fewer paid tokens than the baseline (negative = more); None without a baseline or when it paid nothing.
    pub fn paid_reduction_percent(&self) -> Option<f64> {
        let (b, a) = *self.before_vs_after.as_deref()?;
        if b.paid == 0 {
            return None;
        }
        Some((b.paid as f64 - a.paid as f64) / b.paid as f64 * 100.0)
    }

    /// A short table: one line per turn, the totals, and the after-vs-before line when there is a baseline.
    pub fn text(&self) -> String {
        let mut s = format!("cache simulation ({})\n", self.setup);
        s.push_str("turn    input   cached     paid  written    hit  note\n");
        for t in &self.per_turn {
            let note = match (&t.reason, t.prefix_changed) {
                (Some(r), _) => r.clone(),
                (None, true) => "prefix changed".into(),
                _ => String::new(),
            };
            s.push_str(&format!(
                "{:>4} {:>8} {:>8} {:>8} {:>8} {:>5.0}%  {}\n",
                t.turn,
                t.input_tokens,
                t.cached_tokens,
                t.paid_tokens,
                t.written_tokens,
                t.hit_ratio * 100.0,
                note
            ));
        }
        let t = &self.totals;
        s.push_str(&format!(
            "total {:>7} {:>8} {:>8} {:>8} {:>5.1}%",
            t.input,
            t.cached,
            t.paid,
            t.written,
            t.hit_ratio * 100.0
        ));
        if let Some(c) = t.cost_estimate {
            s.push_str(&format!("  cost ~{c:.4}"));
        }
        s.push('\n');
        if let Some(b) = self.before_vs_after.as_deref() {
            let (before, after) = (b.0, b.1);
            let paid = match self.paid_reduction_percent() {
                Some(p) if p >= 0.0 => format!("{p:.1}% fewer paid tokens"),
                Some(p) => format!("{:.1}% more paid tokens", -p),
                None => "no paid tokens to compare".into(),
            };
            s.push_str(&format!(
                "after vs before: {paid}, hit ratio {:.1}% → {:.1}%\n",
                before.hit_ratio * 100.0,
                after.hit_ratio * 100.0
            ));
        }
        s
    }
}

/// Reads a recorded session from plain JSON, so a CLI can feed the simulator:
///
/// ```json
/// [ { "turn": 1,
///     "prefix":   [ { "id": "tools", "class": "static", "text": "..." } ],
///     "messages": [ { "id": "m1", "role": "user", "text": "...", "tokens": 123 } ] } ]
/// ```
///
/// `class` is "static", "session" or "turn" (any case); `turn` defaults to the request's position (1-based); `prefix`
/// and `messages` default to empty; `tokens` is optional. Anything else is an error that says where.
pub fn from_json(v: &serde_json::Value) -> Result<Vec<SimRequest>, String> {
    let arr = v
        .as_array()
        .ok_or("expected a JSON array of requests".to_string())?;
    let mut out = vec![];
    for (i, r) in arr.iter().enumerate() {
        let at = format!("request #{}", i + 1);
        let o = r.as_object().ok_or(format!("{at}: expected an object"))?;
        let turn = match o.get("turn") {
            None | Some(serde_json::Value::Null) => i as u32 + 1,
            Some(t) => t
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or(format!("{at}: \"turn\" must be a non-negative integer"))?,
        };
        let list = |key: &str| -> Result<Vec<&serde_json::Value>, String> {
            match o.get(key) {
                None | Some(serde_json::Value::Null) => Ok(vec![]),
                Some(serde_json::Value::Array(a)) => Ok(a.iter().collect()),
                Some(_) => Err(format!("{at}: \"{key}\" must be an array")),
            }
        };
        let text_of = |e: &serde_json::Value, key: &str, w: &str| -> Result<String, String> {
            e.get(key)
                .and_then(|x| x.as_str())
                .map(String::from)
                .ok_or(format!("{at}: {w}: missing string field \"{key}\""))
        };
        let mut prefix = vec![];
        for (j, s) in list("prefix")?.into_iter().enumerate() {
            let w = format!("prefix[{j}]");
            let class = match text_of(s, "class", &w)?.to_ascii_lowercase().as_str() {
                "static" => Class::Static,
                "session" => Class::Session,
                "turn" => Class::Turn,
                other => {
                    return Err(format!(
                        "{at}: {w}: unknown class \"{other}\" (static, session or turn)"
                    ))
                }
            };
            prefix.push(Segment::new(
                &text_of(s, "id", &w)?,
                class,
                text_of(s, "text", &w)?,
            ));
        }
        let mut messages = vec![];
        for (j, m) in list("messages")?.into_iter().enumerate() {
            let w = format!("messages[{j}]");
            let tokens = match m.get("tokens") {
                None | Some(serde_json::Value::Null) => None,
                Some(t) => Some(t.as_u64().ok_or(format!(
                    "{at}: {w}: \"tokens\" must be a non-negative integer"
                ))?),
            };
            messages.push(SimMessage {
                id: text_of(m, "id", &w)?,
                role: text_of(m, "role", &w)?,
                text: text_of(m, "text", &w)?,
                tokens,
            });
        }
        out.push(SimRequest {
            turn,
            prefix,
            messages,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Estimator;

    /// 4 chars per token exactly: `words(n)` is n tokens ("abc " per token).
    fn counter() -> Box<dyn Counter> {
        Box::new(Estimator::generic())
    }
    fn words(n: usize) -> String {
        "abc ".repeat(n)
    }
    fn opts(caps: Caps) -> SimOptions {
        SimOptions::new(caps, counter())
    }
    fn msg(id: &str, role: &str, n: usize) -> SimMessage {
        SimMessage::new(id, role, &format!("{id} {}", words(n)))
    }
    /// A growing conversation: turn t has messages m1..m{t}.
    fn session(turns: u32, tools: usize, system: &str) -> Vec<SimRequest> {
        (1..=turns)
            .map(|t| SimRequest {
                turn: t,
                prefix: vec![
                    Segment::new("a-tools", Class::Static, words(tools)),
                    Segment::new("b-system", Class::Static, system.to_string()),
                ],
                messages: (1..=t)
                    .map(|i| {
                        msg(
                            &format!("m{i}"),
                            if i % 2 == 1 { "user" } else { "assistant" },
                            300,
                        )
                    })
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn identical_consecutive_requests_hit_fully_after_the_first() {
        let r = SimRequest {
            turn: 1,
            prefix: vec![Segment::new("tools", Class::Static, words(3000))],
            messages: vec![msg("m1", "user", 200)],
        };
        let reqs: Vec<SimRequest> = (1..=4)
            .map(|t| SimRequest {
                turn: t,
                ..r.clone()
            })
            .collect();
        for caps in [Caps::anthropic(), Caps::openai(), Caps::local()] {
            let rep = simulate(&reqs, &opts(caps));
            assert_eq!(rep.per_turn[0].cached_tokens, 0);
            for t in &rep.per_turn[1..] {
                assert_eq!(t.cached_tokens, t.input_tokens, "{caps:?}");
                assert_eq!(t.paid_tokens, 0);
                assert!((t.hit_ratio - 1.0).abs() < 1e-6);
                assert!(!t.prefix_changed && t.reason.is_none());
            }
        }
    }

    #[test]
    fn a_changed_static_segment_pays_the_whole_turn_and_names_the_segment() {
        let mut reqs = session(3, 2500, &format!("law one. {}", words(10)));
        reqs[2].prefix[1] = Segment::new(
            "b-system",
            Class::Static,
            format!("law one. Latest check (3 min ago): fresh {}", words(10)),
        );
        for caps in [Caps::anthropic(), Caps::openai()] {
            let rep = simulate(&reqs, &opts(caps));
            let t3 = &rep.per_turn[2];
            // the tools segment (2500 tokens) comes first and is unchanged: it is still a hit; everything after is paid
            assert!(t3.prefix_changed, "{caps:?}");
            let why = t3.reason.as_deref().unwrap();
            assert!(
                why.contains("b-system") && why.contains("Static") && why.contains("relative time"),
                "{why}"
            );
            // implicit caches read the unchanged leading segment; Anthropic stored the prefix only at the breakpoint
            // after the last Static segment, and that prefix changed, so it reads nothing
            let want = if caps.explicit_breakpoints > 0 {
                0
            } else {
                2500
            };
            assert_eq!(t3.cached_tokens, want, "{caps:?}");
            assert!(t3.paid_tokens > 0);
            assert!(!rep.per_turn[1].prefix_changed);
        }
        // if the very first segment changes, the whole turn is paid
        let mut first = session(2, 2500, "same");
        first[1].prefix[0] = Segment::new("a-tools", Class::Static, format!("{}x", words(2500)));
        let rep = simulate(&first, &opts(Caps::openai()));
        assert_eq!(rep.per_turn[1].cached_tokens, 0);
        assert_eq!(rep.per_turn[1].paid_tokens, rep.per_turn[1].input_tokens);
        assert!(rep.per_turn[1]
            .reason
            .as_deref()
            .unwrap()
            .contains("a-tools"));
    }

    fn volatile_session() -> Vec<SimRequest> {
        (1..=5)
            .map(|t| SimRequest {
                turn: t,
                prefix: vec![
                    Segment::new("a-tools", Class::Static, words(2000)),
                    Segment::new(
                        "z-memory",
                        Class::Turn,
                        format!("memory updated {t} times {}", words(30)),
                    ),
                ],
                messages: (1..=t)
                    .map(|i| msg(&format!("m{i}"), "user", 400))
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn moving_the_volatile_text_into_the_last_message_raises_the_hit_ratio() {
        let reqs = volatile_session();
        let before = opts(Caps::openai());
        let after = opts(Caps::openai()).with_dynamic_in_turn(true);
        let (b, a) = compare(&reqs, &before, &after);
        assert!(
            a.totals.hit_ratio > b.totals.hit_ratio,
            "{} vs {}",
            a.totals.hit_ratio,
            b.totals.hit_ratio
        );
        assert!(a.totals.paid < b.totals.paid);
        assert!(
            b.per_turn[2]
                .reason
                .as_deref()
                .unwrap()
                .contains("z-memory"),
            "{:?}",
            b.per_turn[2].reason
        );
        assert!(a.before_vs_after.is_some() && b.before_vs_after.is_none());
        assert!(a.paid_reduction_percent().unwrap() > 0.0);
        // the explicit-breakpoint provider gains too
        let (b4, a4) = compare(
            &reqs,
            &opts(Caps::anthropic()),
            &opts(Caps::anthropic()).with_dynamic_in_turn(true),
        );
        assert!(a4.totals.cached > b4.totals.cached);
    }

    #[test]
    fn explicit_breakpoints_look_back_where_implicit_caching_cannot() {
        // turn 2 changes a message in the middle, turn 3 restores it: the explicit cache still holds turn 1's prefix,
        // the implicit one only compares with the request just before
        let mut reqs = session(3, 2500, "sys");
        reqs[1].messages[0].text = format!("edited {}", words(300));
        let anth = simulate(&reqs, &opts(Caps::anthropic()));
        let oai = simulate(&reqs, &opts(Caps::openai()));
        assert!(
            anth.per_turn[2].cached_tokens > oai.per_turn[2].cached_tokens,
            "{} vs {}",
            anth.per_turn[2].cached_tokens,
            oai.per_turn[2].cached_tokens
        );
        // and explicit providers report writes at the breakpoint, implicit ones simply whatever is new
        assert!(anth.per_turn[0].written_tokens > 0 && oai.per_turn[0].written_tokens > 0);
        assert!(anth.per_turn[1].written_tokens < anth.per_turn[1].input_tokens);
    }

    #[test]
    fn min_cacheable_is_respected() {
        let small = |n: u32| SimRequest {
            turn: n,
            prefix: vec![Segment::new("tools", Class::Static, words(600))],
            messages: vec![msg("m1", "user", 100)],
        };
        let reqs = vec![small(1), small(2)];
        for caps in [Caps::anthropic(), Caps::openai()] {
            let rep = simulate(&reqs, &opts(caps));
            assert_eq!(
                rep.per_turn[1].cached_tokens, 0,
                "700 tokens < 1024: never cached, {caps:?}"
            );
            assert_eq!(rep.per_turn[1].written_tokens, 0);
        }
        // local servers cache any size
        let local = simulate(&reqs, &opts(Caps::local()));
        assert_eq!(
            local.per_turn[1].cached_tokens,
            local.per_turn[1].input_tokens
        );
        // Haiku needs 2048: 1500 tokens cache on Sonnet, not on Haiku
        let mid = |n: u32| SimRequest {
            turn: n,
            prefix: vec![Segment::new("tools", Class::Static, words(1500))],
            messages: vec![],
        };
        let two = vec![mid(1), mid(2)];
        assert!(simulate(&two, &opts(Caps::anthropic())).per_turn[1].cached_tokens > 0);
        assert_eq!(
            simulate(&two, &opts(Caps::anthropic_haiku())).per_turn[1].cached_tokens,
            0
        );
    }

    #[test]
    fn totals_add_up_and_cost_uses_the_cache_rates() {
        let reqs = session(5, 3000, "sys");
        let rep = simulate(&reqs, &opts(Caps::anthropic()).with_price(3.0));
        let t = &rep.totals;
        assert_eq!(
            t.input,
            rep.per_turn.iter().map(|x| x.input_tokens).sum::<u64>()
        );
        assert_eq!(
            t.cached,
            rep.per_turn.iter().map(|x| x.cached_tokens).sum::<u64>()
        );
        assert_eq!(t.paid, t.input - t.cached);
        for x in &rep.per_turn {
            assert_eq!(x.paid_tokens, x.input_tokens - x.cached_tokens);
            assert!(x.written_tokens <= x.paid_tokens);
        }
        let want = ((t.paid - t.written) as f64 + t.written as f64 * 1.25 + t.cached as f64 * 0.10)
            * 3.0
            / 1e6;
        assert!((t.cost_estimate.unwrap() - want).abs() < 1e-12);
        assert!(
            t.cost_estimate.unwrap() < t.input as f64 * 3.0 / 1e6,
            "caching must be cheaper than no caching here"
        );
        assert!(simulate(&reqs, &opts(Caps::anthropic()))
            .totals
            .cost_estimate
            .is_none());
    }

    #[test]
    fn shaping_the_tool_outputs_lowers_the_input() {
        let log: String = (0..60)
            .map(|i| format!("   Compiling crate{i} v1.0.{i}\n"))
            .collect::<String>()
            + "error[E0308]: mismatched types\n  --> src/a.rs:1:1\n";
        let reqs: Vec<SimRequest> = (1..=3)
            .map(|t| SimRequest {
                turn: t,
                prefix: vec![Segment::new("tools", Class::Static, words(2000))],
                messages: vec![msg("m1", "user", 50), SimMessage::new("t1", "tool", &log)],
            })
            .collect();
        let (b, a) = compare(
            &reqs,
            &opts(Caps::anthropic()),
            &opts(Caps::anthropic()).with_shape(true),
        );
        assert!(a.totals.input < b.totals.input);
        assert!(a.setup.contains("shaped") && !b.setup.contains("shaped"));
        // shaped once, then stable: the shaped run still hits the cache on turn 2
        assert!(a.per_turn[1].cached_tokens > 0 && a.per_turn[1].reason.is_none());
    }

    #[test]
    fn text_has_a_table_and_the_before_after_line() {
        let reqs = volatile_session();
        let (b, a) = compare(
            &reqs,
            &opts(Caps::openai()),
            &opts(Caps::openai()).with_dynamic_in_turn(true),
        );
        let t = a.text();
        assert!(
            t.contains("after vs before:")
                && t.contains("fewer paid tokens")
                && t.contains("hit ratio")
                && t.contains('→'),
            "{t}"
        );
        assert_eq!(
            t.lines()
                .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
                .count(),
            5,
            "one line per turn\n{t}"
        );
        assert!(!b.text().contains("after vs before:"));
        assert!(t.contains("total"));
    }

    #[test]
    fn from_json_reads_the_documented_shape_and_rejects_the_rest() {
        let v: serde_json::Value = serde_json::from_str(
            r#"[{"turn":4,"prefix":[{"id":"tools","class":"Static","text":"t"},{"id":"mem","class":"turn","text":"m"}],
                 "messages":[{"id":"m1","role":"user","text":"hi","tokens":7}]},
                {"prefix":[],"messages":[]}]"#,
        )
        .unwrap();
        let r = from_json(&v).unwrap();
        assert_eq!((r[0].turn, r[1].turn), (4, 2));
        assert_eq!(r[0].prefix[1].class, Class::Turn);
        assert_eq!(r[0].messages[0].tokens, Some(7));
        let bad = |s: &str| from_json(&serde_json::from_str(s).unwrap()).unwrap_err();
        assert!(bad(r#"{"a":1}"#).contains("array"));
        assert!(bad(r#"[1]"#).contains("request #1") && bad(r#"[1]"#).contains("object"));
        assert!(bad(r#"[{"prefix":[{"id":"x","class":"hot","text":"t"}]}]"#)
            .contains("unknown class \"hot\""));
        assert!(
            bad(r#"[{"messages":[{"id":"m","role":"user"}]}]"#).contains("messages[0]")
                && bad(r#"[{"messages":[{"id":"m","role":"user"}]}]"#).contains("\"text\"")
        );
        assert!(bad(r#"[{"turn":-1}]"#).contains("turn"));
        assert!(bad(r#"[{"prefix":"nope"}]"#).contains("\"prefix\" must be an array"));
        assert!(
            bad(r#"[{"messages":[{"id":"m","role":"u","text":"t","tokens":"x"}]}]"#)
                .contains("tokens")
        );
        // the parsed session simulates
        assert_eq!(simulate(&r, &opts(Caps::local())).per_turn.len(), 2);
    }

    #[test]
    fn the_report_survives_serde() {
        let rep = simulate(
            &session(3, 2500, "sys"),
            &opts(Caps::anthropic()).with_price(1.0),
        );
        let back: SimReport = serde_json::from_str(&serde_json::to_string(&rep).unwrap()).unwrap();
        assert_eq!(back, rep);
    }
}
