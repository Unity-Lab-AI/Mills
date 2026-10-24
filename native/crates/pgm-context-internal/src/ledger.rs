//! The usage ledger: what every turn paid, what the cache gave back, per provider / model / account / session. Fed
//! from the usage objects providers return, in their own shapes, so the hub, the TUI and the orchestrator can show
//! "paid input", "cached input", a hit ratio and a cost, and so every optimisation layer can be measured.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// One turn's usage, normalised. `input` = what was paid at the full input price, `cache_read` = what the cache gave
/// back (Anthropic bills it at 10 %), `cache_write` = what was written into the cache (Anthropic bills 125 %).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64 }

impl Usage {
    pub fn total_input(&self) -> u64 { self.input + self.cache_read + self.cache_write }
    /// Share of the input the cache served: 0.0 (nothing) … 1.0 (everything).
    pub fn hit_ratio(&self) -> f64 { let t = self.total_input(); if t == 0 { 0.0 } else { self.cache_read as f64 / t as f64 } }
    pub fn add(&mut self, o: &Usage) { self.input += o.input; self.output += o.output; self.cache_read += o.cache_read; self.cache_write += o.cache_write; }
}

/// Reads a provider's usage object. The shape says which provider it is:
/// - Anthropic: `input_tokens` (uncached), `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`
/// - OpenAI: `prompt_tokens` (cached included), `completion_tokens`, `prompt_tokens_details.cached_tokens`
/// - Gemini: `promptTokenCount` (cached included), `candidatesTokenCount`, `cachedContentTokenCount`
/// - OpenRouter: the OpenAI shape, sometimes with `cache_discount` (ignored here: a price matter)
/// None when the object has none of these.
pub fn parse_usage(usage: &Value) -> Option<Usage> {
    let n = |v: &Value| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0);
    if usage.get("input_tokens").is_some() || usage.get("cache_read_input_tokens").is_some() {
        return Some(Usage { input: n(&usage["input_tokens"]), output: n(&usage["output_tokens"]), cache_read: n(&usage["cache_read_input_tokens"]), cache_write: n(&usage["cache_creation_input_tokens"]) });
    }
    if usage.get("prompt_tokens").is_some() {
        let cached = n(&usage["prompt_tokens_details"]["cached_tokens"]);
        return Some(Usage { input: n(&usage["prompt_tokens"]).saturating_sub(cached), output: n(&usage["completion_tokens"]), cache_read: cached, cache_write: 0 });
    }
    if usage.get("promptTokenCount").is_some() {
        let cached = n(&usage["cachedContentTokenCount"]);
        return Some(Usage { input: n(&usage["promptTokenCount"]).saturating_sub(cached), output: n(&usage["candidatesTokenCount"]), cache_read: cached, cache_write: 0 });
    }
    None
}

/// Prices per million tokens; `cost` follows Anthropic's rule (cache reads 10 %, writes 125 %) unless the table says
/// otherwise through its own read / write prices.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Prices { pub input_per_m: f64, pub output_per_m: f64, pub cache_read_per_m: f64, pub cache_write_per_m: f64 }
impl Prices {
    pub fn anthropic_style(input_per_m: f64, output_per_m: f64) -> Prices { Prices { input_per_m, output_per_m, cache_read_per_m: input_per_m * 0.1, cache_write_per_m: input_per_m * 1.25 } }
    pub fn free() -> Prices { Prices { input_per_m: 0.0, output_per_m: 0.0, cache_read_per_m: 0.0, cache_write_per_m: 0.0 } }
    pub fn cost(&self, u: &Usage) -> f64 { (u.input as f64 * self.input_per_m + u.output as f64 * self.output_per_m + u.cache_read as f64 * self.cache_read_per_m + u.cache_write as f64 * self.cache_write_per_m) / 1_000_000.0 }
    /// What the same turn would have cost with no cache at all: every input token at the full price.
    pub fn cost_uncached(&self, u: &Usage) -> f64 { (u.total_input() as f64 * self.input_per_m + u.output as f64 * self.output_per_m) / 1_000_000.0 }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn { pub at_ms: u64, pub provider: String, pub model: String, pub account: String, pub session: String, pub usage: Usage }

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals { pub turns: u64, pub usage: Usage }
impl Totals { pub fn hit_ratio(&self) -> f64 { self.usage.hit_ratio() } }

/// The ledger: turns in memory, appended to a JSONL file when one is given.
#[derive(Debug, Default)]
pub struct Ledger { turns: Vec<Turn> }

impl Ledger {
    pub fn new() -> Ledger { Ledger::default() }
    pub fn note(&mut self, turn: Turn) { self.turns.push(turn); }
    /// Note a provider usage object straight away; false when the object had no usage in a known shape.
    pub fn note_usage(&mut self, at_ms: u64, provider: &str, model: &str, account: &str, session: &str, usage: &Value) -> bool {
        match parse_usage(usage) { Some(u) => { self.note(Turn { at_ms, provider: provider.into(), model: model.into(), account: account.into(), session: session.into(), usage: u }); true } None => false }
    }
    pub fn turns(&self) -> &[Turn] { &self.turns }
    pub fn totals(&self, f: impl Fn(&Turn) -> bool) -> Totals {
        let mut t = Totals::default();
        for x in self.turns.iter().filter(|x| f(x)) { t.turns += 1; t.usage.add(&x.usage); }
        t
    }
    pub fn by_provider(&self) -> Vec<(String, Totals)> {
        let mut names: Vec<String> = self.turns.iter().map(|t| t.provider.clone()).collect(); names.sort(); names.dedup();
        names.into_iter().map(|p| { let t = self.totals(|x| x.provider == p); (p, t) }).collect()
    }
    /// Appends a turn to a JSONL file (one line each; the file is the durable ledger).
    pub fn append_jsonl(path: &Path, turn: &Turn) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{}", serde_json::to_string(turn).unwrap_or_default())
    }
    pub fn load_jsonl(path: &Path) -> Ledger {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        Ledger { turns: text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect() }
    }
    /// One line a person can read: turns, input paid / cached, output, hit ratio.
    pub fn summary_line(&self) -> String {
        let t = self.totals(|_| true);
        format!("{} turns · input {} paid + {} cached (+{} written) · output {} · cache hit {:.0}%", t.turns, t.usage.input, t.usage.cache_read, t.usage.cache_write, t.usage.output, t.hit_ratio() * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_three_shapes_normalise_the_same_way() {
        let a = parse_usage(&json!({ "input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 50 })).unwrap();
        assert_eq!(a, Usage { input: 100, output: 20, cache_read: 900, cache_write: 50 });
        let o = parse_usage(&json!({ "prompt_tokens": 1000, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 900 } })).unwrap();
        assert_eq!(o, Usage { input: 100, output: 20, cache_read: 900, cache_write: 0 });
        let g = parse_usage(&json!({ "promptTokenCount": 1000, "candidatesTokenCount": 20, "cachedContentTokenCount": 900 })).unwrap();
        assert_eq!(g, Usage { input: 100, output: 20, cache_read: 900, cache_write: 0 });
        assert!(parse_usage(&json!({ "tokens": 5 })).is_none());
        assert!((a.hit_ratio() - 900.0 / 1050.0).abs() < 1e-9);
    }

    #[test]
    fn totals_hit_ratio_and_cost_follow_the_turns() {
        let mut l = Ledger::new();
        assert!(l.note_usage(1, "anthropic", "claude-haiku-5-5", "sponge", "s1", &json!({ "input_tokens": 1000, "output_tokens": 100 })));
        assert!(l.note_usage(2, "anthropic", "claude-haiku-5-5", "sponge", "s1", &json!({ "input_tokens": 50, "output_tokens": 100, "cache_read_input_tokens": 950 })));
        assert!(l.note_usage(3, "openai", "gpt-5.5", "sponge", "s2", &json!({ "prompt_tokens": 2000, "completion_tokens": 10, "prompt_tokens_details": { "cached_tokens": 1500 } })));
        assert!(!l.note_usage(4, "x", "y", "z", "s3", &json!({})));
        let all = l.totals(|_| true);
        assert_eq!(all.turns, 3); assert_eq!(all.usage.input, 1000 + 50 + 500); assert_eq!(all.usage.cache_read, 950 + 1500);
        let ant = l.totals(|t| t.provider == "anthropic");
        assert_eq!(ant.turns, 2); assert!((ant.hit_ratio() - 950.0 / 2000.0).abs() < 1e-9);
        let p = Prices::anthropic_style(1.0, 5.0);
        let u = &l.turns()[1].usage;
        assert!((p.cost(u) - (50.0 * 1.0 + 100.0 * 5.0 + 950.0 * 0.1) / 1e6).abs() < 1e-12);
        assert!(p.cost(u) < p.cost_uncached(u));
        assert_eq!(l.by_provider().len(), 2);
        assert!(l.summary_line().contains("3 turns"));
    }

    #[test]
    fn the_jsonl_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("pgm-ledger-{}", std::process::id()));
        let f = dir.join("ledger.jsonl");
        let t = Turn { at_ms: 9, provider: "openai".into(), model: "gpt-5.5".into(), account: "a".into(), session: "s".into(), usage: Usage { input: 1, output: 2, cache_read: 3, cache_write: 0 } };
        Ledger::append_jsonl(&f, &t).unwrap(); Ledger::append_jsonl(&f, &t).unwrap();
        let l = Ledger::load_jsonl(&f);
        assert_eq!(l.turns().len(), 2); assert_eq!(l.turns()[0], t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
