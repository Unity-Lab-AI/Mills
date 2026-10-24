//! The savings report (Jcode-Mods DECISIONS item 58): what shaping kept out of the context per tool and per session,
//! and how well the prompt cache served the input. Pure: the caller reads the ledger file and hands the records in.
//!
//! A ledger line is a [`Turn`] (unchanged) plus an optional `shaped` array, so lines written before this module existed
//! still parse (`shaped` is simply empty) and [`Turn`] / [`Ledger`](crate::ledger::Ledger) keep their API. The line a
//! writer appends is `TurnRecord { turn, shaped }`, flattened:
//! `{"at_ms":..,"provider":..,"model":..,"account":..,"session":..,"usage":{..},"shaped":[{"tool":"bash","original_tokens":900,"shaped_tokens":120,"stashed":true}]}`.
use crate::ledger::{Turn, Usage};
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::BTreeMap;

/// One tool output as shaped before it entered the context, in tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapedOutput {
    pub tool: String,
    pub original_tokens: u64,
    pub shaped_tokens: u64,
    /// True when the full output was stashed (reversible through its handle).
    #[serde(default)]
    pub stashed: bool,
}

impl ShapedOutput {
    pub fn new(
        tool: &str,
        original_tokens: u64,
        shaped_tokens: u64,
        stashed: bool,
    ) -> ShapedOutput {
        ShapedOutput {
            tool: tool.into(),
            original_tokens,
            shaped_tokens,
            stashed,
        }
    }
    /// Tokens kept out of the context; never negative (a shaped output that grew saved nothing).
    pub fn saved(&self) -> u64 {
        self.original_tokens.saturating_sub(self.shaped_tokens)
    }
}

/// One ledger line: the turn's usage plus what shaping did during it. Old lines (no `shaped`) parse with it empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    #[serde(flatten)]
    pub turn: Turn,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shaped: Vec<ShapedOutput>,
}

impl TurnRecord {
    pub fn new(turn: Turn, shaped: Vec<ShapedOutput>) -> TurnRecord {
        TurnRecord { turn, shaped }
    }
    pub fn parse_line(line: &str) -> Option<TurnRecord> {
        serde_json::from_str(line).ok()
    }
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    /// Every parseable line of a JSONL text; bad lines are skipped.
    pub fn parse_lines(text: &str) -> Vec<TurnRecord> {
        text.lines().filter_map(TurnRecord::parse_line).collect()
    }
}

fn pct(saved: u64, original: u64) -> f64 {
    if original == 0 {
        0.0
    } else {
        saved as f64 * 100.0 / original as f64
    }
}
fn ratio(usage: &Usage) -> f64 {
    usage.hit_ratio()
}

/// Savings of one tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolSavings {
    pub tool: String,
    pub calls: u64,
    pub stashed: u64,
    pub original_tokens: u64,
    pub shaped_tokens: u64,
    pub saved_tokens: u64,
    pub saved_pct: f64,
}

/// Savings and cache use of one session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionSavings {
    pub session: String,
    pub turns: u64,
    pub calls: u64,
    pub original_tokens: u64,
    pub shaped_tokens: u64,
    pub saved_tokens: u64,
    pub saved_pct: f64,
    pub usage: Usage,
    pub hit_ratio: f64,
}

/// The whole report: per tool (most saved first), per session (most saved first), the cache hit ratio and totals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SavingsReport {
    pub turns: u64,
    pub calls: u64,
    pub stashed: u64,
    pub original_tokens: u64,
    pub shaped_tokens: u64,
    pub saved_tokens: u64,
    pub saved_pct: f64,
    pub usage: Usage,
    /// cache_read / (input + cache_read + cache_write), 0.0 when nothing was read.
    pub hit_ratio: f64,
    pub tools: Vec<ToolSavings>,
    pub sessions: Vec<SessionSavings>,
}

#[derive(Default)]
struct Acc {
    calls: u64,
    stashed: u64,
    original: u64,
    shaped: u64,
    saved: u64,
}
impl Acc {
    fn add(&mut self, s: &ShapedOutput) {
        self.calls += 1;
        if s.stashed {
            self.stashed += 1;
        }
        self.original += s.original_tokens;
        self.shaped += s.shaped_tokens;
        self.saved += s.saved();
    }
}

impl SavingsReport {
    pub fn from_records<I, R>(records: I) -> SavingsReport
    where
        I: IntoIterator<Item = R>,
        R: Borrow<TurnRecord>,
    {
        let mut tools: BTreeMap<String, Acc> = BTreeMap::new();
        let mut sessions: BTreeMap<String, (u64, Acc, Usage)> = BTreeMap::new();
        let (mut total, mut usage, mut turns) = (Acc::default(), Usage::default(), 0u64);
        for r in records {
            let r = r.borrow();
            turns += 1;
            usage.add(&r.turn.usage);
            let s = sessions.entry(r.turn.session.clone()).or_default();
            s.0 += 1;
            s.2.add(&r.turn.usage);
            for o in &r.shaped {
                tools.entry(o.tool.clone()).or_default().add(o);
                s.1.add(o);
                total.add(o);
            }
        }
        let mut tools: Vec<ToolSavings> = tools
            .into_iter()
            .map(|(tool, a)| ToolSavings {
                tool,
                calls: a.calls,
                stashed: a.stashed,
                original_tokens: a.original,
                shaped_tokens: a.shaped,
                saved_tokens: a.saved,
                saved_pct: pct(a.saved, a.original),
            })
            .collect();
        tools.sort_by(|a, b| {
            b.saved_tokens
                .cmp(&a.saved_tokens)
                .then_with(|| a.tool.cmp(&b.tool))
        });
        // records with no session id still count in the totals but are not listed as a session
        let mut sessions: Vec<SessionSavings> = sessions
            .into_iter()
            .filter(|(k, _)| !k.is_empty())
            .map(|(session, (turns, a, usage))| SessionSavings {
                session,
                turns,
                calls: a.calls,
                original_tokens: a.original,
                shaped_tokens: a.shaped,
                saved_tokens: a.saved,
                saved_pct: pct(a.saved, a.original),
                hit_ratio: ratio(&usage),
                usage,
            })
            .collect();
        sessions.sort_by(|a, b| {
            b.saved_tokens
                .cmp(&a.saved_tokens)
                .then_with(|| a.session.cmp(&b.session))
        });
        SavingsReport {
            turns,
            calls: total.calls,
            stashed: total.stashed,
            original_tokens: total.original,
            shaped_tokens: total.shaped,
            saved_tokens: total.saved,
            saved_pct: pct(total.saved, total.original),
            hit_ratio: ratio(&usage),
            usage,
            tools,
            sessions,
        }
    }

    /// A compact text table for `/context report`.
    pub fn render(&self) -> String {
        let mut out = format!(
            "Context savings · {} turns · {} shaped outputs ({} stashed)\n",
            self.turns, self.calls, self.stashed
        );
        out.push_str(&format!(
            "  {:<18} {:>6} {:>10} {:>10} {:>10} {:>6}\n",
            "tool", "calls", "original", "shaped", "saved", "%"
        ));
        for t in &self.tools {
            out.push_str(&format!(
                "  {:<18} {:>6} {:>10} {:>10} {:>10} {:>5.0}%\n",
                t.tool, t.calls, t.original_tokens, t.shaped_tokens, t.saved_tokens, t.saved_pct
            ));
        }
        if self.tools.is_empty() {
            out.push_str("  (no shaped tool outputs recorded)\n");
        }
        out.push_str(&format!(
            "  {:<18} {:>6} {:>10} {:>10} {:>10} {:>5.0}%\n",
            "total",
            self.calls,
            self.original_tokens,
            self.shaped_tokens,
            self.saved_tokens,
            self.saved_pct
        ));
        if !self.sessions.is_empty() {
            out.push_str("  sessions:\n");
            for s in &self.sessions {
                out.push_str(&format!(
                    "    {:<18} {:>4} turns · saved {} of {} ({:.0}%) · cache hit {:.0}%\n",
                    s.session,
                    s.turns,
                    s.saved_tokens,
                    s.original_tokens,
                    s.saved_pct,
                    s.hit_ratio * 100.0
                ));
            }
        }
        out.push_str(&format!(
            "  cache: {} paid + {} read + {} written input → hit {:.0}%\n",
            self.usage.input,
            self.usage.cache_read,
            self.usage.cache_write,
            self.hit_ratio * 100.0
        ));
        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(session: &str, usage: (u64, u64, u64), shaped: Vec<ShapedOutput>) -> TurnRecord {
        TurnRecord::new(
            Turn {
                at_ms: 1,
                provider: "anthropic".into(),
                model: "m".into(),
                account: "a".into(),
                session: session.into(),
                usage: Usage {
                    input: usage.0,
                    output: 10,
                    cache_read: usage.1,
                    cache_write: usage.2,
                },
            },
            shaped,
        )
    }
    fn fixture() -> Vec<TurnRecord> {
        vec![
            rec(
                "s1",
                (100, 800, 100),
                vec![
                    ShapedOutput::new("bash", 1000, 200, true),
                    ShapedOutput::new("read", 500, 500, false),
                ],
            ),
            rec(
                "s1",
                (50, 900, 50),
                vec![ShapedOutput::new("bash", 600, 100, false)],
            ),
            rec(
                "s2",
                (200, 0, 0),
                vec![ShapedOutput::new("grep", 300, 250, false)],
            ),
        ]
    }

    #[test]
    fn old_ledger_lines_still_parse() {
        let old = r#"{"at_ms":9,"provider":"openai","model":"gpt-5.5","account":"a","session":"s","usage":{"input":1,"output":2,"cache_read":3,"cache_write":0}}"#;
        let r = TurnRecord::parse_line(old).unwrap();
        assert!(r.shaped.is_empty());
        assert_eq!(r.turn.session, "s");
        assert_eq!(r.turn.usage.cache_read, 3);
        // and the old reader still reads new lines (unknown field ignored)
        let new = rec("s", (1, 2, 3), vec![ShapedOutput::new("bash", 10, 2, true)]).to_line();
        assert!(serde_json::from_str::<Turn>(&new).is_ok());
    }

    #[test]
    fn a_record_round_trips_and_omits_empty_shaped() {
        let r = rec("s", (1, 2, 3), vec![ShapedOutput::new("bash", 10, 2, true)]);
        assert_eq!(TurnRecord::parse_line(&r.to_line()).unwrap(), r);
        assert!(!rec("s", (1, 2, 3), vec![]).to_line().contains("shaped"));
        let partial = r#"{"at_ms":1,"provider":"p","model":"m","account":"","session":"s","usage":{"input":1,"output":1,"cache_read":0,"cache_write":0},"shaped":[{"tool":"bash","original_tokens":5,"shaped_tokens":1}]}"#;
        assert!(!TurnRecord::parse_line(partial).unwrap().shaped[0].stashed);
    }

    #[test]
    fn parse_lines_skips_bad_lines() {
        let text = format!(
            "{}\nnot json\n\n{}\n",
            fixture()[0].to_line(),
            fixture()[2].to_line()
        );
        assert_eq!(TurnRecord::parse_lines(&text).len(), 2);
    }

    #[test]
    fn aggregation_is_exact_per_tool() {
        let r = SavingsReport::from_records(fixture());
        assert_eq!(r.turns, 3);
        assert_eq!(r.calls, 4);
        assert_eq!(r.stashed, 1);
        assert_eq!(
            (r.original_tokens, r.shaped_tokens, r.saved_tokens),
            (2400, 1050, 1350)
        );
        let bash = r.tools.iter().find(|t| t.tool == "bash").unwrap();
        assert_eq!(
            (
                bash.calls,
                bash.stashed,
                bash.original_tokens,
                bash.shaped_tokens,
                bash.saved_tokens
            ),
            (2, 1, 1600, 300, 1300)
        );
        assert!((bash.saved_pct - 81.25).abs() < 1e-9);
        let read = r.tools.iter().find(|t| t.tool == "read").unwrap();
        assert_eq!(read.saved_tokens, 0);
        assert_eq!(read.saved_pct, 0.0);
        assert!((r.saved_pct - 56.25).abs() < 1e-9);
    }

    #[test]
    fn tools_and_sessions_sort_by_saved_descending() {
        let r = SavingsReport::from_records(fixture());
        let order: Vec<&str> = r.tools.iter().map(|t| t.tool.as_str()).collect();
        assert_eq!(order, vec!["bash", "grep", "read"]);
        let s: Vec<(&str, u64)> = r
            .sessions
            .iter()
            .map(|s| (s.session.as_str(), s.saved_tokens))
            .collect();
        assert_eq!(s, vec![("s1", 1300), ("s2", 50)]);
    }

    #[test]
    fn ties_sort_by_name() {
        let r = SavingsReport::from_records(vec![rec(
            "s",
            (1, 0, 0),
            vec![
                ShapedOutput::new("b", 10, 5, false),
                ShapedOutput::new("a", 10, 5, false),
            ],
        )]);
        assert_eq!(r.tools[0].tool, "a");
    }

    #[test]
    fn per_session_and_overall_hit_ratio() {
        let r = SavingsReport::from_records(fixture());
        let s1 = r.sessions.iter().find(|s| s.session == "s1").unwrap();
        assert_eq!(s1.turns, 2);
        assert_eq!(s1.usage.cache_read, 1700);
        assert!((s1.hit_ratio - 1700.0 / 2000.0).abs() < 1e-9);
        assert_eq!(
            r.sessions
                .iter()
                .find(|s| s.session == "s2")
                .unwrap()
                .hit_ratio,
            0.0
        );
        assert!((r.hit_ratio - 1700.0 / 2200.0).abs() < 1e-9);
    }

    #[test]
    fn empty_input_is_zero_safe() {
        let r = SavingsReport::from_records(Vec::<TurnRecord>::new());
        assert_eq!((r.turns, r.calls, r.saved_tokens), (0, 0, 0));
        assert_eq!(r.saved_pct, 0.0);
        assert_eq!(r.hit_ratio, 0.0);
        assert!(r.render().contains("no shaped tool outputs"));
        assert!(r.to_json()["tools"].as_array().unwrap().is_empty());
    }

    #[test]
    fn zero_token_outputs_and_growth_do_not_divide_or_underflow() {
        let r = SavingsReport::from_records(vec![rec(
            "s",
            (0, 0, 0),
            vec![
                ShapedOutput::new("x", 0, 0, false),
                ShapedOutput::new("y", 10, 30, false),
            ],
        )]);
        assert!(r.tools.iter().all(|t| t.saved_pct.is_finite()));
        assert_eq!(r.saved_tokens, 0);
        assert_eq!(r.hit_ratio, 0.0);
    }

    #[test]
    fn records_without_a_session_count_in_totals_only() {
        let r = SavingsReport::from_records(vec![rec(
            "",
            (10, 0, 0),
            vec![ShapedOutput::new("x", 10, 0, false)],
        )]);
        assert!(r.sessions.is_empty());
        assert_eq!(r.saved_tokens, 10);
    }

    #[test]
    fn from_records_takes_references_too() {
        let f = fixture();
        assert_eq!(
            SavingsReport::from_records(&f).saved_tokens,
            SavingsReport::from_records(f.clone()).saved_tokens
        );
    }

    #[test]
    fn render_names_every_tool_and_the_cache() {
        let text = SavingsReport::from_records(fixture()).render();
        for t in [
            "bash",
            "read",
            "grep",
            "total",
            "s1",
            "s2",
            "cache hit",
            "77%",
        ] {
            assert!(text.contains(t), "{t} missing in\n{text}");
        }
        assert!(text.find("bash").unwrap() < text.find("grep").unwrap());
    }

    #[test]
    fn to_json_carries_the_numbers() {
        let j = SavingsReport::from_records(fixture()).to_json();
        assert_eq!(j["saved_tokens"], 1350);
        assert_eq!(j["tools"][0]["tool"], "bash");
        assert_eq!(j["sessions"][0]["session"], "s1");
        assert!(j["hit_ratio"].as_f64().unwrap() > 0.7);
    }
}
