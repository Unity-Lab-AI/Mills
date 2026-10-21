//! Usage (moved from bridge/usage.js): every Claude account's 5-hour / 7-day windows (this hub's own login "hub", and
//! each AI host's Claude "h:<hostId>/<provider>"), and each person's context window. Claude Code reports the limits in
//! its stream (rate-limit events); a relayed call's Anthropic headers carry the exact numbers, which win. Until one was
//! seen, the windows are estimated from when the account started being used. State: bridge/usage.json (the same file).
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

use vr_core::now_ms;

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
    pub fn new(root: &std::path::Path, cfg: &super::Config, persist: bool) -> Usage {
        let file = root.join("usage.json");
        let mut db: Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        if !db.is_object() { db = json!({}); }
        if !db["sources"].is_object() { db["sources"] = json!({}); }
        Usage { file, db: Mutex::new(db), context_window: cfg.get("contextWindow").and_then(Value::as_u64).unwrap_or(200000), persist }
    }
    fn save(&self, db: &Value) { if !self.persist { return; } let tmp = self.file.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(db).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp, &self.file); } }
    fn with_bucket<R>(&self, source: &str, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut db = self.db.lock().unwrap();
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
            let key = if ["seven", "week", "7d"].iter().any(|k| kind.contains(k)) { Some("seven_day") } else if ["five", "5h", "session"].iter().any(|k| kind.contains(k)) { Some("five_hour") } else { None };
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
        let db = self.db.lock().unwrap(); let now = now_ms();
        let b = if source.is_empty() || source == "hub" { &*db } else { &db["sources"][source] };
        let ok = |l: &Value| if l["exact"].as_bool() == Some(true) && l["resetsAt"].as_u64().map(|r| r > now).unwrap_or(false) { json!({ "utilization": l["utilization"], "resetsAt": l["resetsAt"], "status": l["status"] }) } else { Value::Null };
        json!({ "five_hour": ok(&b["limits"]["five_hour"]), "seven_day": ok(&b["limits"]["seven_day"]) })
    }
    /// The /api/usage answer: the windows of the source the person's provider uses, and their context (ctx, session).
    pub fn status(&self, source: &str, ctx: &Value, session: &str) -> Value {
        let db = self.db.lock().unwrap(); let now = now_ms();
        let b = if source.is_empty() || source == "hub" { (*db).clone() } else { db["sources"][source].clone() };
        let win = |key: &str, start: &Value, len: u64| -> Value {
            let l = &b["limits"][key];
            if l["resetsAt"].as_u64().map(|r| r > now).unwrap_or(false) { return json!({ "source": "claude", "status": l["status"], "utilization": l["utilization"], "resetsAt": l["resetsAt"] }); }
            let s = start.as_u64().filter(|s| now - s < len);
            let rejected = l["status"] == json!("rejected") && l["at"].as_u64().map(|a| now - a < len).unwrap_or(false);
            json!({ "source": "estimate", "status": if rejected { "rejected" } else { "allowed" }, "utilization": Value::Null, "resetsAt": s.map(|s| s + len) })
        };
        let c = if ctx.is_object() && ctx["session"].as_str().unwrap_or("") == session { json!({ "tokens": ctx["tokens"], "window": ctx["window"].as_u64().unwrap_or(self.context_window), "model": ctx["model"] }) } else { Value::Null };
        json!({ "now": now, "of": if source.is_empty() { "hub" } else { source }, "five_hour": win("five_hour", &b["fiveStart"], FIVE_H), "seven_day": win("seven_day", &b["weekStart"], SEVEN_D), "context": c })
    }
}
/// The context a run's event shows (input + cache tokens of the latest reply; the window from the result).
pub fn note_context(ctx: &mut Value, ev: &Value, session: &str) {
    if ev["type"] == "assistant" && ev["message"]["usage"].is_object() {
        let u = &ev["message"]["usage"];
        let tokens = u["input_tokens"].as_u64().unwrap_or(0) + u["cache_read_input_tokens"].as_u64().unwrap_or(0) + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        let mut c = if ctx.is_object() { ctx.clone() } else { json!({}) };
        c["session"] = json!(if session.is_empty() { ev["session_id"].as_str().unwrap_or("") } else { session }); c["tokens"] = json!(tokens); c["model"] = json!(ev["message"]["model"].as_str().unwrap_or("")); c["at"] = json!(now_ms());
        *ctx = c;
    }
    if ev["type"] == "result" { if let Some(m) = ev["modelUsage"].as_object() { let w = m.values().filter_map(|x| x["contextWindow"].as_u64()).max(); if let (Some(w), true) = (w, ctx.is_object()) { ctx["window"] = json!(w); } } }
}
