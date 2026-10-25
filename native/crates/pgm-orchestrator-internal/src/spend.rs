//! Spend per worker (DECISIONS A1 item 6): tokens and cost summed per worker from usage events, for `status` and the
//! tool's model list. A cost is only ever computed from prices the worker carries (per million tokens, USD micros);
//! an unknown price stays unknown ("unpriced"), it is never guessed. Free workers cost 0 by definition.
use crate::profile::Worker;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub worker: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// USD micros if the provider reported it or it was computed; None = unknown
    pub cost_usd_micros: Option<u64>,
}

impl Usage {
    pub fn new(worker: &str, input: u64, output: u64) -> Usage {
        Usage {
            worker: worker.into(),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd_micros: None,
        }
    }
    pub fn cache(mut self, read: u64, write: u64) -> Usage {
        self.cache_read_tokens = read;
        self.cache_write_tokens = write;
        self
    }
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

/// The cost of a usage from the worker's per-million prices. Free workers cost 0. None when a category that was
/// used has no price (cache tokens are not priced as input: that would be inventing).
pub fn cost_micros(w: &Worker, u: &Usage) -> Option<u64> {
    if w.free && w.route_says_free() {
        return Some(0);
    }
    let part = |tokens: u64, price: Option<u64>| -> Option<u128> {
        if tokens == 0 {
            Some(0)
        } else {
            price.map(|p| tokens as u128 * p as u128)
        }
    };
    let sum = part(u.input_tokens, w.prompt_price_micros_per_m)?
        + part(u.output_tokens, w.completion_price_micros_per_m)?
        + part(u.cache_read_tokens, w.cache_read_price_micros_per_m)?
        + part(u.cache_write_tokens, w.cache_write_price_micros_per_m)?;
    // round to the nearest micro
    Some(((sum + 500_000) / 1_000_000).min(u64::MAX as u128) as u64)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSpend {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// sum of the known costs, USD micros
    pub known_cost_micros: u64,
    pub priced_calls: u32,
    pub unpriced_calls: u32,
}

impl WorkerSpend {
    fn add(&mut self, u: &Usage) {
        self.input_tokens += u.input_tokens;
        self.output_tokens += u.output_tokens;
        self.cache_read_tokens += u.cache_read_tokens;
        self.cache_write_tokens += u.cache_write_tokens;
        match u.cost_usd_micros {
            Some(c) => {
                self.known_cost_micros += c;
                self.priced_calls += 1;
            }
            None => self.unpriced_calls += 1,
        }
    }
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
    /// "$0.0123", "$0.0123 + unpriced" (some calls unknown), or "unpriced" (none known)
    pub fn cost_text(&self) -> String {
        match (self.priced_calls, self.unpriced_calls) {
            (0, 0) => "$0.0000".into(),
            (0, _) => "unpriced".into(),
            (_, 0) => usd(self.known_cost_micros),
            _ => format!("{} + unpriced", usd(self.known_cost_micros)),
        }
    }
}

fn usd(micros: u64) -> String {
    let u = (micros as u128 + 50) / 100;
    format!("${}.{:04}", u / 10_000, u % 10_000)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendLedger {
    pub workers: BTreeMap<String, WorkerSpend>,
}

impl SpendLedger {
    pub fn new() -> SpendLedger {
        SpendLedger::default()
    }
    /// Adds a usage event. A missing cost is computed from `worker`'s prices when given; it stays None otherwise.
    pub fn record(&mut self, mut usage: Usage, worker: Option<&Worker>) {
        if usage.cost_usd_micros.is_none() {
            if let Some(w) = worker {
                usage.cost_usd_micros = cost_micros(w, &usage);
            }
        }
        self.workers
            .entry(usage.worker.clone())
            .or_default()
            .add(&usage);
    }
    pub fn of(&self, route: &str) -> WorkerSpend {
        self.workers.get(route).cloned().unwrap_or_default()
    }
    pub fn total(&self) -> WorkerSpend {
        let mut t = WorkerSpend::default();
        for w in self.workers.values() {
            t.input_tokens += w.input_tokens;
            t.output_tokens += w.output_tokens;
            t.cache_read_tokens += w.cache_read_tokens;
            t.cache_write_tokens += w.cache_write_tokens;
            t.known_cost_micros += w.known_cost_micros;
            t.priced_calls += w.priced_calls;
            t.unpriced_calls += w.unpriced_calls;
        }
        t
    }
    /// A compact table for `status`.
    pub fn render(&self) -> String {
        if self.workers.is_empty() {
            return "spend: nothing yet".into();
        }
        let mut out = String::from("worker | in | out | cache r/w | cost\n");
        for (r, s) in &self.workers {
            out.push_str(&format!(
                "{r} | {} | {} | {}/{} | {}\n",
                s.input_tokens,
                s.output_tokens,
                s.cache_read_tokens,
                s.cache_write_tokens,
                s.cost_text()
            ));
        }
        let t = self.total();
        out.push_str(&format!(
            "total | {} | {} | {}/{} | {}",
            t.input_tokens,
            t.output_tokens,
            t.cache_read_tokens,
            t.cache_write_tokens,
            t.cost_text()
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::TaskKind;

    fn paid() -> Worker {
        Worker::new("claude-api:haiku")
            .with(200_000, &[TaskKind::General], false)
            .priced(1_000_000, 5_000_000)
    } // $1/M in, $5/M out
    fn free() -> Worker {
        Worker::new("openrouter:a/b:free").with(100_000, &[TaskKind::General], true)
    }

    #[test]
    fn cost_comes_from_per_million_prices() {
        // 200k in at $1/M = $0.20, 10k out at $5/M = $0.05
        assert_eq!(
            cost_micros(&paid(), &Usage::new("claude-api:haiku", 200_000, 10_000)),
            Some(250_000)
        );
        assert_eq!(
            cost_micros(&paid(), &Usage::new("claude-api:haiku", 0, 0)),
            Some(0)
        );
    }

    #[test]
    fn free_workers_cost_nothing_and_unknown_prices_stay_unknown() {
        assert_eq!(
            cost_micros(&free(), &Usage::new("x", 1_000, 1_000)),
            Some(0)
        );
        let unknown = Worker::new("claude-api:opus").with(200_000, &[TaskKind::General], false);
        assert_eq!(
            cost_micros(&unknown, &Usage::new("claude-api:opus", 10, 10)),
            None
        );
        // cache tokens used but no cache price: unknown, not priced as input
        assert_eq!(
            cost_micros(&paid(), &Usage::new("claude-api:haiku", 10, 10).cache(5, 0)),
            None
        );
        assert_eq!(
            cost_micros(&paid(), &Usage::new("claude-api:haiku", 10, 10).cache(0, 0)),
            Some(60)
        );
    }

    #[test]
    fn the_ledger_sums_per_worker_and_in_total() {
        let mut l = SpendLedger::new();
        let (p, f) = (paid(), free());
        l.record(Usage::new("claude-api:haiku", 100_000, 1_000), Some(&p));
        l.record(
            Usage::new("claude-api:haiku", 100_000, 1_000).cache(7, 3),
            Some(&Worker {
                cache_read_price_micros_per_m: Some(100_000),
                cache_write_price_micros_per_m: Some(1_250_000),
                ..p.clone()
            }),
        );
        l.record(Usage::new("openrouter:a/b:free", 500, 50), Some(&f));
        let h = l.of("claude-api:haiku");
        assert_eq!(
            (
                h.input_tokens,
                h.output_tokens,
                h.cache_read_tokens,
                h.cache_write_tokens
            ),
            (200_000, 2_000, 7, 3)
        );
        assert_eq!(h.priced_calls, 2);
        assert_eq!(l.of("openrouter:a/b:free").known_cost_micros, 0);
        let t = l.total();
        assert_eq!(
            (
                t.input_tokens,
                t.output_tokens,
                t.priced_calls,
                t.unpriced_calls
            ),
            (200_500, 2_050, 3, 0)
        );
        assert_eq!(t.known_cost_micros, h.known_cost_micros);
        assert_eq!(t.total_tokens(), 200_500 + 2_050 + 10);
    }

    #[test]
    fn a_reported_cost_wins_over_the_computed_one() {
        let mut l = SpendLedger::new();
        let mut u = Usage::new("claude-api:haiku", 1_000_000, 0);
        u.cost_usd_micros = Some(42);
        l.record(u, Some(&paid()));
        assert_eq!(l.of("claude-api:haiku").known_cost_micros, 42);
    }

    #[test]
    fn unpriced_stays_unpriced_in_the_render() {
        let mut l = SpendLedger::new();
        assert_eq!(l.render(), "spend: nothing yet");
        l.record(Usage::new("mystery:m", 10, 5), None);
        l.record(
            Usage::new("mystery:m", 10, 5),
            Some(&Worker::new("mystery:m")),
        );
        let r = l.render();
        assert!(r.contains("mystery:m | 20 | 10 | 0/0 | unpriced"), "{r}");
        assert!(r.lines().last().unwrap().ends_with("| unpriced"), "{r}");
        assert_eq!(l.total().known_cost_micros, 0, "unknown is not zero");
        l.record(Usage::new("claude-api:haiku", 1_000_000, 0), Some(&paid()));
        let r = l.render();
        assert!(
            r.lines().last().unwrap().ends_with("| $1.0000 + unpriced"),
            "{r}"
        );
        assert!(
            r.contains("claude-api:haiku | 1000000 | 0 | 0/0 | $1.0000"),
            "{r}"
        );
    }

    #[test]
    fn usd_formats_four_decimals() {
        assert_eq!(usd(0), "$0.0000");
        assert_eq!(usd(250_000), "$0.2500");
        assert_eq!(usd(12_300), "$0.0123");
        assert_eq!(usd(3_500_000), "$3.5000");
    }
}
