//! Usage (moved from bridge/usage.js): every Claude account's 5-hour / 7-day windows (this hub's own login "hub", and
//! each AI host's Claude "h:<hostId>/<provider>"), and each person's context window. Claude Code reports the limits in
//! its stream (rate-limit events); a relayed call's Anthropic headers carry the exact numbers, which win. Until one was
//! seen, the windows are estimated from when the account started being used. State: bridge/usage.json (the same file).
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

use pgm_context_internal::ledger::Usage as LedgerUsage;
use pgm_core_internal::now_ms;

const FIVE_H: u64 = 5 * 3600 * 1000;
const SEVEN_D: u64 = 7 * 86400 * 1000;

fn to_ms(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_f64().map(|x| if x < 1e12 { (x * 1000.0) as u64 } else { x as u64 }),
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis() as u64).or_else(|| s.parse::<f64>().ok().map(|x| if x < 1e12 { (x * 1000.0) as u64 } else { x as u64 })),
        _ => None,
    }
}
fn num(v: &Value) -> Option<f64> { match v { Value::Number(n) => n.as_f64(), Value::String(s) if !s.is_empty() => s.parse().ok(), _ => None } }

/// persist: whether this hub writes usage.json. Not while the JavaScript hub runs Work (it owns the file then, and hears
/// every host window from here); this copy then keeps what it saw in memory, on top of the file as it was at start.
pub struct Usage { file: PathBuf, db: Mutex<Value>, context_window: u64, persist: bool }
impl Usage {
    pub fn new(root: &std::path::Path, cfg: &pgm_core_internal::config::Config, persist: bool) -> Usage {
        let file = root.join("usage.json");
        let mut db: Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        if !db.is_object() { db = json!({}); }
        if !db["sources"].is_object() { db["sources"] = json!({}); }
        Usage { file, db: Mutex::new(db), context_window: cfg.get("contextWindow").and_then(Value::as_u64).unwrap_or(200000), persist }
    }
    fn save(&self, db: &Value) { if !self.persist { return; } let tmp = self.file.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(db).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp, &self.file); } }
    fn with_bucket<R>(&self, source: &str, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let r = if source.is_empty() || source == "hub" { f(&mut db) } else {
            if !db["sources"][source].is_object() { db["sources"][source] = json!({}); }
            f(&mut db["sources"][source])
        };
        self.save(&db);
        r
    }
    /// A Work run on that source's Claude started.
    pub fn note_activity(&self, source: &str) {
        self.with_bucket(source, |b| { let now = now_ms(); if b["fiveStart"].as_u64().map(|s| now - s >= FIVE_H).unwrap_or(true) { b["fiveStart"] = json!(now); } if b["weekStart"].as_u64().map(|s| now - s >= SEVEN_D).unwrap_or(true) { b["weekStart"] = json!(now); } });
    }
    /// One stream event of a Work run: the windows, and the person's context (returned for their Work state).
    pub fn note_event(&self, ev: &Value, source: &str) {
        let t = ev["type"].as_str().unwrap_or("");
        if t.contains("rate_limit") {
            let info = if ev.get("rate_limit_info").is_some() { ev["rate_limit_info"].clone() } else if ev.get("rateLimitInfo").is_some() { ev["rateLimitInfo"].clone() } else { ev.clone() };
            let kind = info["rateLimitType"].as_str().or(info["rate_limit_type"].as_str()).or(info["type"].as_str()).unwrap_or("").to_lowercase();
            // only the all-models windows (not seven_day_opus / _sonnet / _overage_included)
            let key = match kind.as_str() { "seven_day" | "weekly" | "7d" => Some("seven_day"), "five_hour" | "5h" | "session" => Some("five_hour"), _ => None };
            if let Some(key) = key {
                self.with_bucket(source, |b| {
                    if !b["limits"].is_object() { b["limits"] = json!({}); }
                    let prev = b["limits"][key].clone(); let now = now_ms();
                    let resets = to_ms(&info["resetsAt"]).or(to_ms(&info["resets_at"])).or(prev["resetsAt"].as_u64().filter(|r| *r > now));
                    let raw = num(&info["utilization"]).or(num(&info["usage"]));
                    let same = prev["resetsAt"].as_u64().is_some() && resets == prev["resetsAt"].as_u64();
                    if prev["exact"].as_bool() == Some(true) && prev["resetsAt"].as_u64().map(|r| r > now).unwrap_or(false) && raw.is_none() {
                        if let Some(s) = info["status"].as_str() { b["limits"][key]["status"] = json!(s); }
                        return;
                    }
                    let util = raw.map(|u| (if u > 1.0 { u / 100.0 } else { u }).clamp(0.0, 1.0)).or(if same { prev["utilization"].as_f64() } else { None });
                    b["limits"][key] = json!({ "status": info["status"].as_str().map(String::from).unwrap_or_else(|| if same { prev["status"].as_str().unwrap_or("").to_string() } else { String::new() }), "utilization": util, "resetsAt": resets, "at": now });
                });
            }
        }
        if t == "result" && ["rate limit", "rate_limit", "ratelimit", "usage limit"].iter().any(|k| ev["result"].as_str().unwrap_or("").to_lowercase().contains(k)) {
            self.with_bucket(source, |b| { if !b["limits"].is_object() { b["limits"] = json!({}); } let mut f = b["limits"]["five_hour"].clone(); if !f.is_object() { f = json!({}); } f["status"] = json!("rejected"); f["at"] = json!(now_ms()); b["limits"]["five_hour"] = f; });
        }
    }
    /// One answer outside Work (the /v1 API, Unity, a product's AI), normalised by pgm-context's ledger: added to that
    /// person's total for the provider kind (usage.json "answers": { person: { kind: { turns, input, output, cached,
    /// written, model, at } } }). `/api/usage` shows the person's sum as `answers`.
    pub fn note_answer(&self, person: &str, kind: &str, model: &str, u: &LedgerUsage) {
        let (person, kind) = (if person.is_empty() { "hub" } else { person }, if kind.is_empty() { "unknown" } else { kind });
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        if !db["answers"].is_object() { db["answers"] = json!({}); }
        if !db["answers"][person].is_object() { db["answers"][person] = json!({}); }
        let mut t = db["answers"][person][kind].clone(); if !t.is_object() { t = json!({}); }
        let add = |t: &mut Value, n: &str, v: u64| { let cur = t[n].as_u64().unwrap_or(0); t[n] = json!(cur + v); };
        add(&mut t, "turns", 1); add(&mut t, "input", u.input); add(&mut t, "output", u.output); add(&mut t, "cached", u.cache_read); add(&mut t, "written", u.cache_write);
        t["model"] = json!(model); t["at"] = json!(now_ms());
        db["answers"][person][kind] = t;
        self.save(&db);
    }
    /// A person's answers outside Work, summed over their providers (Null when none yet): turns, input (paid), output,
    /// cached (read from the cache), written (into it), hit = cached / (input + cached + written), and `by` per kind.
    pub fn answers(&self, person: &str) -> Value {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let Some(kinds) = db["answers"][person].as_object().filter(|k| !k.is_empty()) else { return Value::Null };
        let mut sum = LedgerUsage::default(); let mut turns = 0u64; let mut by = serde_json::Map::new();
        for (k, t) in kinds {
            let g = |n: &str| t[n].as_u64().unwrap_or(0);
            let u = LedgerUsage { input: g("input"), output: g("output"), cache_read: g("cached"), cache_write: g("written") };
            sum.add(&u); turns += g("turns");
            by.insert(k.clone(), json!({ "turns": g("turns"), "input": u.input, "output": u.output, "cached": u.cache_read, "written": u.cache_write, "hit": u.hit_ratio(), "model": t["model"] }));
        }
        json!({ "turns": turns, "input": sum.input, "output": sum.output, "cached": sum.cache_read, "written": sum.cache_write, "hit": sum.hit_ratio(), "by": Value::Object(by) })
    }
    /// Anthropic's own numbers for one source (the relayed calls' headers): exact, kept, they win.
    pub fn note_windows(&self, source: &str, w: &Value) {
        self.with_bucket(source, |b| {
            if !b["limits"].is_object() { b["limits"] = json!({}); }
            for key in ["five_hour", "seven_day"] {
                let x = &w[key]; let (Some(u), Some(r)) = (num(&x["utilization"]), to_ms(&x["resetsAt"])) else { continue };
                b["limits"][key] = json!({ "status": x["status"].as_str().unwrap_or(""), "utilization": (if u > 1.0 { u / 100.0 } else { u }).clamp(0.0, 1.0), "resetsAt": r, "at": now_ms(), "exact": true });
            }
        });
    }
    /// The exact windows of a source (null when not known or reset).
    pub fn windows(&self, source: &str) -> Value {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner()); let now = now_ms();
        let b = if source.is_empty() || source == "hub" { &*db } else { &db["sources"][source] };
        let ok = |l: &Value| if l["exact"].as_bool() == Some(true) && l["resetsAt"].as_u64().map(|r| r > now).unwrap_or(false) { json!({ "utilization": l["utilization"], "resetsAt": l["resetsAt"], "status": l["status"], "at": l["at"] }) } else { Value::Null };
        json!({ "five_hour": ok(&b["limits"]["five_hour"]), "seven_day": ok(&b["limits"]["seven_day"]) })
    }
    /// The /api/usage answer: the windows of the source the person's provider uses, and their context (ctx, session).
    pub fn status(&self, source: &str, ctx: &Value, session: &str) -> Value {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner()); let now = now_ms();
        let b = if source.is_empty() || source == "hub" { (*db).clone() } else { db["sources"][source].clone() };
        let win = |key: &str, start: &Value, len: u64| -> Value {
            let l = &b["limits"][key];
            if l["resetsAt"].as_u64().map(|r| r > now).unwrap_or(false) { return json!({ "source": "claude", "status": l["status"], "utilization": l["utilization"], "resetsAt": l["resetsAt"] }); }
            let s = start.as_u64().filter(|s| now - s < len);
            let rejected = l["status"] == json!("rejected") && l["at"].as_u64().map(|a| now - a < len).unwrap_or(false);
            json!({ "source": "estimate", "status": if rejected { "rejected" } else { "allowed" }, "utilization": Value::Null, "resetsAt": s.map(|s| s + len) })
        };
        // the session's cache ledger (TOKENS.md): paid vs cached input over its turns, and the hit ratio
        let cache = if ctx["cache"].is_object() { let k = &ctx["cache"]; let g = |n: &str| k[n].as_u64().unwrap_or(0); let total = g("paid") + g("read") + g("write"); json!({ "turns": g("turns"), "paid": g("paid"), "read": g("read"), "write": g("write"), "hit": if total == 0 { 0.0 } else { g("read") as f64 / total as f64 } }) } else { Value::Null };
        let c = if ctx.is_object() && ctx["session"].as_str().unwrap_or("") == session { json!({ "tokens": ctx["tokens"], "window": context_window(ctx, self.context_window), "model": ctx["model"], "cache": cache }) } else { Value::Null };
        json!({ "now": now, "of": if source.is_empty() { "hub" } else { source }, "five_hour": win("five_hour", &b["fiveStart"], FIVE_H), "seven_day": win("seven_day", &b["weekStart"], SEVEN_D), "context": c })
    }
}
/// The larger window a model can have (Claude's 1M context).
const LARGE_WINDOW: u64 = 1_000_000;
/// The window to show for a context (T-302): what the run's result reported (else the configured default), at least 1M for
/// a model id tagged `[1m]`, and never less than what the session already holds. A 1M model whose result says 200 000, or
/// a long first run before any result, used to show 200 %+ of a 200k window; past the window it is corrected to the next
/// tier (1M), and past that to the tokens themselves.
pub fn context_window(ctx: &Value, default: u64) -> u64 {
    let mut w = ctx["window"].as_u64().filter(|w| *w > 0).unwrap_or(default);
    if ctx["model"].as_str().map(|m| m.to_ascii_lowercase().contains("[1m]")).unwrap_or(false) { w = w.max(LARGE_WINDOW); }
    let tokens = ctx["tokens"].as_u64().unwrap_or(0);
    if tokens > w { w = if tokens <= LARGE_WINDOW { LARGE_WINDOW } else { tokens }; }
    w
}
/// jcode's token counts in Claude's usage shape (`input_tokens` without the cache, `cache_read_input_tokens`,
/// `cache_creation_input_tokens`, `output_tokens`; T-388). jcode names them `input`, `output`, `cache_read_input`,
/// `cache_creation_input` (its `run --ndjson`), the Jcode PGM Server `input`, `output`, `cacheRead`, `cacheWrite`. An
/// OpenAI-shaped count (OpenRouter's `prompt_tokens`, what jcode reports for every provider but Anthropic's) already holds the
/// cached part: `prompt_includes_cache` takes it out once, so the context is never counted twice.
pub fn claude_usage(u: &Value, prompt_includes_cache: bool) -> Value {
    let g = |a: &str, b: &str| u[a].as_u64().or_else(|| u[b].as_u64()).unwrap_or(0);
    let (input, output, read, write) = (g("input", "input_tokens"), g("output", "output_tokens"), g("cache_read_input", "cacheRead"), g("cache_creation_input", "cacheWrite"));
    json!({ "input_tokens": if prompt_includes_cache { input.saturating_sub(read + write) } else { input }, "output_tokens": output, "cache_read_input_tokens": read, "cache_creation_input_tokens": write })
}
/// The context a run's event shows (input + cache tokens of the latest reply; the window from the result). Also a `usage`
/// event (T-388: a Jcode PGM Server session's latest count, never shown on the page): `usage` in Claude's shape, and
/// optionally `model` and `contextWindow` (the model's own, from OpenRouter's catalogue).
pub fn note_context(ctx: &mut Value, ev: &Value, session: &str) {
    let usage = if ev["type"] == "assistant" && ev["message"]["usage"].is_object() { Some((&ev["message"]["usage"], ev["message"]["model"].as_str().unwrap_or(""))) }
        else if ev["type"] == "usage" && ev["usage"].is_object() { Some((&ev["usage"], ev["model"].as_str().unwrap_or(""))) } else { None };
    if let Some((u, model)) = usage {
        let tokens = u["input_tokens"].as_u64().unwrap_or(0) + u["cache_read_input_tokens"].as_u64().unwrap_or(0) + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        let mut c = if ctx.is_object() { ctx.clone() } else { json!({}) };
        let sid = if session.is_empty() { ev["session_id"].as_str().unwrap_or("") } else { session };
        // the cache ledger of this session: paid input, cache reads, cache writes, turns (a new session starts it again)
        let mut k = if c["session"].as_str() == Some(sid) && c["cache"].is_object() { c["cache"].clone() } else { json!({ "turns": 0, "paid": 0, "read": 0, "write": 0 }) };
        let add = |k: &mut Value, n: &str, v: u64| { let cur = k[n].as_u64().unwrap_or(0); k[n] = json!(cur + v); };
        add(&mut k, "turns", 1); add(&mut k, "paid", u["input_tokens"].as_u64().unwrap_or(0)); add(&mut k, "read", u["cache_read_input_tokens"].as_u64().unwrap_or(0)); add(&mut k, "write", u["cache_creation_input_tokens"].as_u64().unwrap_or(0));
        let same = c["session"].as_str() == Some(sid);
        c["session"] = json!(sid); c["tokens"] = json!(tokens); c["at"] = json!(now_ms()); c["cache"] = k;
        if !model.is_empty() || !same { c["model"] = json!(model); } // a usage event without a model keeps the session's
        if let Some(w) = ev["contextWindow"].as_u64().filter(|w| *w > 0) { c["window"] = json!(w); }
        *ctx = c;
    }
    if ev["type"] == "result" { if let Some(m) = ev["modelUsage"].as_object() { let w = m.values().filter_map(|x| x["contextWindow"].as_u64()).max(); if let (Some(w), true) = (w, ctx.is_object()) { ctx["window"] = json!(w); } } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgm_context_internal::ledger::parse_usage;

    #[test]
    fn a_usage_event_fills_the_context_with_its_model_and_window_and_counts_an_openai_prompt_once() {
        // the Jcode PGM Server's count (OpenRouter: prompt_tokens 30 000 of which 25 000 cached)
        let u = claude_usage(&json!({ "input": 30_000, "output": 900, "cacheRead": 25_000, "cacheWrite": 0, "promptIncludesCache": true }), true);
        assert_eq!(u, json!({ "input_tokens": 5_000, "output_tokens": 900, "cache_read_input_tokens": 25_000, "cache_creation_input_tokens": 0 }));
        assert_eq!(claude_usage(&json!({ "input": 100, "cache_read_input": 400 }), false)["input_tokens"], 100, "Anthropic's input is without the cache");
        let mut ctx = Value::Null;
        note_context(&mut ctx, &json!({ "type": "usage", "usage": u, "model": "deepseek/deepseek-v4-flash", "contextWindow": 1_048_576 }), "s1");
        assert_eq!((ctx["tokens"].clone(), ctx["model"].clone(), ctx["window"].clone(), ctx["session"].clone()), (json!(30_000), json!("deepseek/deepseek-v4-flash"), json!(1_048_576), json!("s1")));
        // the next count of the same session without a model keeps the model; the cache ledger adds up
        note_context(&mut ctx, &json!({ "type": "usage", "usage": { "input_tokens": 1_000, "cache_read_input_tokens": 31_000 } }), "s1");
        assert_eq!((ctx["tokens"].clone(), ctx["model"].clone(), ctx["cache"]["turns"].clone(), ctx["cache"]["read"].clone()), (json!(32_000), json!("deepseek/deepseek-v4-flash"), json!(2), json!(56_000)));
        assert_eq!(context_window(&ctx, 200_000), 1_048_576, "the model's own window, not the default");
        note_context(&mut ctx, &json!({ "type": "usage", "usage": "not an object" }), "s1"); assert_eq!(ctx["tokens"], 32_000, "nothing else changes it");
    }
    #[test]
    fn only_the_all_models_windows_count_for_the_caps() {
        // a host refused at 43 % "over its 90 % weekly cap": another weekly limit overwrote the all-models week
        let root = std::env::temp_dir().join(format!("vr-usage-kinds-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root); std::fs::create_dir_all(&root).unwrap();
        let cfg = pgm_core_internal::config::Config { raw: Default::default(), port: 0 };
        let u = Usage::new(&root, &cfg, false);
        let at = (now_ms() / 1000 + 86_400) as f64;
        u.note_event(&json!({ "type": "rate_limit_event", "rate_limit_info": { "rateLimitType": "seven_day", "status": "allowed", "utilization": 0.43, "resetsAt": at } }), "h:x/claude");
        for k in ["seven_day_overage_included", "seven_day_opus", "seven_day_sonnet", "five_hour_overage"] {
            u.note_event(&json!({ "type": "rate_limit_event", "rate_limit_info": { "rateLimitType": k, "status": "allowed_warning", "utilization": 0.95, "resetsAt": at } }), "h:x/claude");
        }
        let s = u.status("h:x/claude", &json!(null), "");
        assert!((s["seven_day"]["utilization"].as_f64().unwrap() - 0.43).abs() < 1e-9, "{s}");
        assert!(s["five_hour"]["utilization"].is_null(), "{s}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_window_is_never_below_what_the_session_holds() {
        // T-302: a 1M model shown as 200k with 200 %+ (Gee, 2026-10-25)
        assert_eq!(context_window(&json!({ "tokens": 50_000, "window": 200_000 }), 200_000), 200_000);
        assert_eq!(context_window(&json!({ "tokens": 50_000 }), 200_000), 200_000, "no result yet: the configured default");
        assert_eq!(context_window(&json!({ "tokens": 450_000, "window": 200_000 }), 200_000), 1_000_000, "past 200k: the 1M tier");
        assert_eq!(context_window(&json!({ "tokens": 450_000 }), 200_000), 1_000_000, "a long first run before any result");
        assert_eq!(context_window(&json!({ "tokens": 10_000, "model": "claude-opus-5-5[1m]" }), 200_000), 1_000_000, "the [1m] tag");
        assert_eq!(context_window(&json!({ "tokens": 1_200_000, "window": 1_000_000 }), 200_000), 1_200_000, "never above 100 %");
        assert_eq!(context_window(&json!({ "tokens": 10_000, "window": 0 }), 200_000), 200_000, "a zero window is no window");
    }

    #[test]
    fn answers_are_summed_per_person_and_provider_and_survive_a_restart() {
        let root = std::env::temp_dir().join(format!("vr-usage-answers-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root); std::fs::create_dir_all(&root).unwrap();
        let cfg = pgm_core_internal::config::Config { raw: Default::default(), port: 0 };
        let u = Usage::new(&root, &cfg, true);
        assert!(u.answers("sponge").is_null(), "no answers yet: the key is left out of /api/usage");
        let a = parse_usage(&json!({ "prompt_tokens": 1000, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 900 } })).unwrap();
        let b = parse_usage(&json!({ "input_tokens": 100, "output_tokens": 10, "cache_read_input_tokens": 600, "cache_creation_input_tokens": 300 })).unwrap();
        u.note_answer("sponge", "openai", "gpt-x", &a); u.note_answer("sponge", "anthropic", "claude-y", &b); u.note_answer("sponge", "openai", "gpt-x", &a); u.note_answer("other", "openai", "gpt-x", &a);
        let s = u.answers("sponge");
        assert_eq!((s["turns"].as_u64(), s["input"].as_u64(), s["output"].as_u64(), s["cached"].as_u64(), s["written"].as_u64()), (Some(3), Some(300), Some(50), Some(2400), Some(300)));
        assert!((s["hit"].as_f64().unwrap() - 2400.0 / 3000.0).abs() < 1e-9);
        assert_eq!(s["by"]["openai"]["turns"], json!(2)); assert!((s["by"]["openai"]["hit"].as_f64().unwrap() - 0.9).abs() < 1e-9);
        assert_eq!(s["by"]["anthropic"]["written"], json!(300)); assert_eq!(s["by"]["anthropic"]["model"], json!("claude-y"));
        assert_eq!(u.answers("other")["turns"], json!(1));
        let again = Usage::new(&root, &cfg, true);
        assert_eq!(again.answers("sponge")["cached"], json!(2400), "kept in usage.json");
        let _ = std::fs::remove_dir_all(&root);
    }
}
