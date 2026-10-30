//! Logs, metrics and traces (T-331; team note plans/sessions-always-saved.md § 4): what happened, when, where, how long
//! and why, for the hub and any program on the PGM libraries.
//! - **Logs**: one JSON line per event, `{ at, level, area, what, …fields }`, in `<dir>/<name>-YYYY-MM-DD.jsonl` (one file a
//!   day, kept `KEEP_DAYS`), written through the write buffer (`store`), so logging never waits on the disk. A field
//!   whose name says it is secret (token, secret, password, key, authorization, cookie) never reaches the file, and a
//!   long text is cut: logs say what happened, not what was said.
//! - **Metrics**: counters and latency histograms by name and labels, in memory (`count`, `observe_ms`), read as JSON
//!   (`snapshot`) or Prometheus text (`prometheus`).
//! - **Traces**: every line about one run (or one request) carries its id, so `trace("run", id)` lists them in order.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const KEEP_DAYS: i64 = 14;
const BUCKETS_MS: [f64; 12] = [1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 10000.0, 60000.0];

struct Sink { dir: PathBuf, name: String }
fn sink() -> &'static Mutex<Option<Sink>> { static S: OnceLock<Mutex<Option<Sink>>> = OnceLock::new(); S.get_or_init(Mutex::default) }
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

fn started() -> &'static std::time::Instant { static T: OnceLock<std::time::Instant> = OnceLock::new(); T.get_or_init(std::time::Instant::now) }
/// Seconds since `init` (or the first metric).
pub fn uptime_s() -> u64 { started().elapsed().as_secs() }
/// Where the logs go (`<dir>/<name>-YYYY-MM-DD.jsonl`); files older than `KEEP_DAYS` there are removed now.
pub fn init(dir: &Path, name: &str) {
    let _ = started();
    let _ = std::fs::create_dir_all(dir);
    let _ = crate::fs::private_dir(dir);
    *lock(sink()) = Some(Sink { dir: dir.to_path_buf(), name: name.to_string() });
    prune(dir, name, crate::now_ms() as i64);
}
fn day_of(ms: i64) -> String { crate::time::iso_at(ms).chars().take(10).collect() }
fn file_for(dir: &Path, name: &str, ms: i64) -> PathBuf { dir.join(format!("{name}-{}.jsonl", day_of(ms))) }
fn prune(dir: &Path, name: &str, now: i64) {
    let keep: Vec<String> = (0..KEEP_DAYS).map(|d| format!("{name}-{}.jsonl", day_of(now - d * 86_400_000))).collect();
    if let Ok(rd) = std::fs::read_dir(dir) { for e in rd.flatten() { let n = e.file_name().to_string_lossy().to_string(); if n.starts_with(&format!("{name}-")) && n.ends_with(".jsonl") && !keep.contains(&n) { let _ = std::fs::remove_file(e.path()); } } }
}

/// A key that names a secret: its value never reaches a log.
pub fn secret_key(k: &str) -> bool { let k = k.to_ascii_lowercase(); ["token", "secret", "password", "passwd", "apikey", "api_key", "authorization", "cookie", "credential", "privatekey"].iter().any(|s| k.contains(s)) || k == "key" || k.ends_with("_key") || k.ends_with("key") && k.len() > 3 && !k.ends_with("monkey") }
/// Fields made safe to log: secrets out, long texts cut (300 characters), nesting kept (to 4 levels).
pub fn clean(v: &Value) -> Value { clean_d(v, 0) }
fn clean_d(v: &Value, d: usize) -> Value {
    match v {
        Value::Object(m) if d < 4 => Value::Object(m.iter().filter(|(k, _)| !secret_key(k)).map(|(k, x)| (k.clone(), clean_d(x, d + 1))).collect()),
        Value::Object(_) | Value::Array(_) if d >= 4 => json!("…"),
        Value::Array(a) => Value::Array(a.iter().take(50).map(|x| clean_d(x, d + 1)).collect()),
        Value::String(s) if s.chars().count() > 300 => json!(format!("{}… ({} characters)", s.chars().take(300).collect::<String>(), s.chars().count())),
        o => o.clone(),
    }
}
/// One log line: `level` (info / warn / error), `area` (work, relay, sync, http, …), `what` happened, and its fields
/// (ids: run, session, request, user; took_ms; why …). Nothing happens before `init`.
pub fn log(level: &str, area: &str, what: &str, fields: Value) {
    let Some((dir, name)) = lock(sink()).as_ref().map(|s| (s.dir.clone(), s.name.clone())) else { return };
    let at = crate::now_ms() as i64;
    let mut line = Map::new();
    line.insert("at".into(), json!(crate::time::iso_at(at)));
    line.insert("level".into(), json!(level)); line.insert("area".into(), json!(area)); line.insert("what".into(), json!(what));
    if let Value::Object(m) = clean(&fields) { for (k, v) in m { if !line.contains_key(&k) { line.insert(k, v); } } }
    crate::store::global().append_line(&file_for(&dir, &name, at), &Value::Object(line));
}
pub fn info(area: &str, what: &str, fields: Value) { log("info", area, what, fields) }
pub fn warn(area: &str, what: &str, fields: Value) { log("warn", area, what, fields) }
pub fn error(area: &str, what: &str, fields: Value) { log("error", area, what, fields) }

/// Every line of today's and yesterday's log whose `field` is `id` (a run's trace: `trace("run", id)`), oldest first.
pub fn trace(field: &str, id: &str) -> Vec<Value> {
    let Some((dir, name)) = lock(sink()).as_ref().map(|s| (s.dir.clone(), s.name.clone())) else { return vec![] };
    crate::store::global().flush();
    let now = crate::now_ms() as i64;
    [now - 86_400_000, now].iter().flat_map(|t| crate::store::read_lines(&file_for(&dir, &name, *t))).filter(|l| l[field] == json!(id)).collect()
}

// ---------- metrics ----------
#[derive(Default)]
struct Hist { n: u64, sum: f64, max: f64, buckets: [u64; 12] }
#[derive(Default)]
struct Reg { counters: BTreeMap<String, u64>, hists: BTreeMap<String, Hist> }
fn reg() -> &'static Mutex<Reg> { static R: OnceLock<Mutex<Reg>> = OnceLock::new(); R.get_or_init(Mutex::default) }
/// `name{a="x",b="y"}`, the labels sorted (one series per label set).
fn series(name: &str, labels: &[(&str, &str)]) -> String {
    if labels.is_empty() { return name.to_string(); }
    let mut l: Vec<(&str, &str)> = labels.to_vec(); l.sort();
    format!("{name}{{{}}}", l.iter().map(|(k, v)| format!("{k}=\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))).collect::<Vec<_>>().join(","))
}
/// A counter goes up by one.
pub fn count(name: &str, labels: &[(&str, &str)]) { add(name, labels, 1) }
pub fn add(name: &str, labels: &[(&str, &str)], n: u64) { *lock(reg()).counters.entry(series(name, labels)).or_default() += n; }
/// A latency (ms) into its histogram.
pub fn observe_ms(name: &str, labels: &[(&str, &str)], ms: f64) {
    let mut r = lock(reg()); let h = r.hists.entry(series(name, labels)).or_default();
    h.n += 1; h.sum += ms; if ms > h.max { h.max = ms; }
    for (i, b) in BUCKETS_MS.iter().enumerate() { if ms <= *b { h.buckets[i] += 1; } }
}
/// Every counter and histogram (count, average, max, p50 / p95 from the buckets).
pub fn snapshot() -> Value {
    let r = lock(reg());
    let q = |h: &Hist, f: f64| -> f64 { let want = (h.n as f64 * f).ceil() as u64; BUCKETS_MS.iter().zip(h.buckets.iter()).find(|(_, c)| **c >= want).map(|(b, _)| *b).unwrap_or(h.max) };
    json!({
        "counters": r.counters.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<String, Value>>(),
        "latency": r.hists.iter().map(|(k, h)| (k.clone(), json!({ "count": h.n, "avgMs": if h.n > 0 { (h.sum / h.n as f64 * 10.0).round() / 10.0 } else { 0.0 }, "maxMs": (h.max * 10.0).round() / 10.0, "p50Ms": q(h, 0.5), "p95Ms": q(h, 0.95) }))).collect::<Map<String, Value>>(),
    })
}
/// The same in Prometheus text format (counters as `_total`, histograms as `_ms` buckets).
pub fn prometheus() -> String {
    let r = lock(reg()); let mut out = String::new();
    let split = |s: &str| -> (String, String) { match s.find('{') { Some(i) => (s[..i].to_string(), s[i + 1..s.len() - 1].to_string()), None => (s.to_string(), String::new()) } };
    for (k, v) in &r.counters { let (n, l) = split(k); out.push_str(&format!("{n}_total{} {v}\n", if l.is_empty() { String::new() } else { format!("{{{l}}}") })); }
    for (k, h) in &r.hists {
        let (n, l) = split(k); let lab = |extra: &str| if l.is_empty() { format!("{{{extra}}}") } else { format!("{{{l},{extra}}}") };
        for (i, b) in BUCKETS_MS.iter().enumerate() { out.push_str(&format!("{n}_ms_bucket{} {}\n", lab(&format!("le=\"{b}\"")), h.buckets[i])); }
        out.push_str(&format!("{n}_ms_bucket{} {}\n{n}_ms_sum{} {}\n{n}_ms_count{} {}\n", lab("le=\"+Inf\""), h.n, if l.is_empty() { String::new() } else { format!("{{{l}}}") }, h.sum, if l.is_empty() { String::new() } else { format!("{{{l}}}") }, h.n));
    }
    out
}
/// A route made general for metrics: ids and long numbers become `:id`, the query goes.
pub fn route_of(path: &str) -> String {
    let p = path.split('?').next().unwrap_or("");
    p.split('/').map(|seg| {
        let digits = seg.chars().filter(|c| c.is_ascii_digit()).count();
        let hexish = seg.len() >= 12 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        if hexish || (digits >= 3 && digits * 2 >= seg.len()) || seg.starts_with("s-") && seg.len() > 6 || seg.len() > 40 { ":id" } else { seg }
    }).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_never_reach_a_log_line_and_long_texts_are_cut() {
        let v = clean(&json!({ "user": "ann", "token": "abc", "relayToken": "x", "password": "p", "apiKey": "k", "authorization": "Bearer y", "nested": { "secret": 1, "ok": 2 }, "text": "x".repeat(400), "monkey": "fine" }));
        assert_eq!(v["user"], json!("ann")); assert_eq!(v["monkey"], json!("fine")); assert_eq!(v["nested"], json!({ "ok": 2 }));
        for k in ["token", "relayToken", "password", "apiKey", "authorization"] { assert!(v.get(k).is_none(), "{k} kept"); }
        assert!(v["text"].as_str().unwrap().contains("(400 characters)"));
    }
    #[test]
    fn a_runs_lines_come_back_as_its_trace_and_old_days_go() {
        let d = std::env::temp_dir().join(format!("pgm-obs-{}", crate::crypto::random_hex(4)));
        std::fs::create_dir_all(&d).unwrap();
        let old = d.join(format!("t-{}.jsonl", day_of(crate::now_ms() as i64 - 30 * 86_400_000))); std::fs::write(&old, "{}\n").unwrap();
        init(&d, "t");
        assert!(!old.exists(), "a log past KEEP_DAYS is removed");
        info("work", "run started", json!({ "run": "r1", "user": "ann", "token": "never" }));
        info("relay", "call", json!({ "run": "r2" }));
        error("work", "run failed", json!({ "run": "r1", "why": "exit 1" }));
        let t = trace("run", "r1");
        assert_eq!(t.len(), 2); assert_eq!(t[0]["what"], json!("run started")); assert_eq!(t[1]["why"], json!("exit 1"));
        assert!(t[0].get("token").is_none() && t[0]["at"].is_string() && t[0]["level"] == json!("info"));
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn metrics_count_and_time_by_series() {
        for ms in [3.0, 8.0, 40.0, 900.0] { observe_ms("t_req", &[("route", "/api/x"), ("status", "200")], ms); }
        count("t_runs", &[("harness", "claude")]); count("t_runs", &[("harness", "claude")]);
        let s = snapshot();
        assert_eq!(s["counters"]["t_runs{harness=\"claude\"}"], json!(2));
        let h = &s["latency"]["t_req{route=\"/api/x\",status=\"200\"}"]; assert_eq!(h["count"], json!(4)); assert_eq!(h["maxMs"], json!(900.0)); assert_eq!(h["p50Ms"], json!(10.0));
        let p = prometheus(); assert!(p.contains("t_runs_total{harness=\"claude\"} 2") && p.contains("t_req_ms_count{route=\"/api/x\",status=\"200\"} 4"));
        assert_eq!(route_of("/api/work/sessions/s-b407838c1c30/open?x=1"), "/api/work/sessions/:id/open");
        assert_eq!(route_of("/api/board/task"), "/api/board/task");
        assert_eq!(route_of("/llm/0123456789abcdef0123456789abcdef0123456789abcdef/v1/messages"), "/llm/:id/v1/messages");
    }
}
