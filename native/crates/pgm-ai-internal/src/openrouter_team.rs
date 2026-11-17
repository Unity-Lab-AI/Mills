//! The team OpenRouter key's own limits and what is left on the account (T-381, team note plans/openrouter-team-admin.md).
//!
//! **Limits for everyone together** on the team key, on top of each person's month budget (openrouter.rs): the last
//! 5 hours (rolling), the last 7 days (rolling, like Claude's 5h / 7d bars in Work) and the calendar month (UTC); defaults
//! $10, $20 and $20. At or over one, every team-key request is refused before OpenRouter is asked ([`refusal`]), naming
//! the window, the cap and when it frees up. A person's own key is never counted here. An admin sets them (Manage AI);
//! when the key itself has a limit on OpenRouter, a cap above it is refused (the hub cannot spend more than the key may),
//! while an unlimited key may get caps above the account's balance (no auto top-up: the balance is the real stop, and the
//! page shows it).
//!
//! **The ledger** (`<root>/openrouter-team-ledger.jsonl`, Rust-only, one line per team-key answer: `{ at, usd, user,
//! session?, model? }`, older than 40 days dropped when it is read at start) is kept in memory too; the windows are summed
//! from it. Settings in `<root>/openrouter-team.json` (owner-only): `{ caps: { fiveHourUsd, weekUsd, monthUsd },
//! managementKey? }`.
//!
//! **What is left**: `GET /key` (the key's own limit, what it spent today / this week / this month, the budget that blocks
//! it first) and `GET /credits` (the account's credits and use: OpenRouter asks for a management key; the team key is
//! tried first, then the optional management key). Both are rate-limited per address by OpenRouter, so they are asked at
//! most once a minute.
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::hub::{answer, AiHub, Resp};
use pgm_core_internal::json::{read_json, s, write_json_private as write_json};
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

pub const DEFAULT_CAPS: (f64, f64, f64) = (10.0, 20.0, 20.0);
const H5: u64 = 5 * 3600 * 1000;
const D7: u64 = 7 * 24 * 3600 * 1000;
const KEEP: u64 = 40 * 24 * 3600 * 1000;
const ASK_EVERY: Duration = Duration::from_secs(60);

fn settings_file(hub: &dyn AiHub) -> PathBuf { hub.root().join("openrouter-team.json") }
fn ledger_file(hub: &dyn AiHub) -> PathBuf { hub.root().join("openrouter-team-ledger.jsonl") }
fn settings(hub: &dyn AiHub) -> Value { read_json(&settings_file(hub)).filter(Value::is_object).unwrap_or_else(|| json!({})) }
fn cap_in(v: &Value, k: &str, d: f64) -> f64 { v["caps"][k].as_f64().filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(d) }
/// (5 h, 7 d, month) in USD.
pub fn caps(hub: &dyn AiHub) -> (f64, f64, f64) { let v = settings(hub); (cap_in(&v, "fiveHourUsd", DEFAULT_CAPS.0), cap_in(&v, "weekUsd", DEFAULT_CAPS.1), cap_in(&v, "monthUsd", DEFAULT_CAPS.2)) }
fn usd(x: f64) -> f64 { (x * 1_000_000.0).round() / 1_000_000.0 }

// ---------- the ledger ----------
#[derive(Clone)]
pub struct Line { pub at: u64, pub usd: f64, pub user: String, pub session: String }
struct Ledger { root: PathBuf, lines: Vec<Line> }
static LEDGER: LazyLock<Mutex<Option<Ledger>>> = LazyLock::new(|| Mutex::new(None));
/// The ledger of this hub, read from its file the first time (and again if the hub's root changed: the suite's hubs).
fn with_ledger<T>(hub: &dyn AiHub, f: impl FnOnce(&mut Ledger) -> T) -> T {
    let mut g = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().map(|l| l.root != hub.root()).unwrap_or(true) {
        let cut = now_ms().saturating_sub(KEEP);
        let lines = std::fs::read_to_string(ledger_file(hub)).unwrap_or_default().lines().filter_map(|t| serde_json::from_str::<Value>(t).ok())
            .map(|v| Line { at: v["at"].as_u64().unwrap_or(0), usd: v["usd"].as_f64().unwrap_or(0.0), user: s(&v, "user"), session: s(&v, "session") }).filter(|l| l.at >= cut).collect();
        *g = Some(Ledger { root: hub.root().to_path_buf(), lines });
    }
    f(g.as_mut().expect("just set"))
}
/// One team-key answer's cost goes into the ledger (in memory and on disk).
pub fn record(hub: &dyn AiHub, user: &str, cost: f64, session: &str) {
    if !cost.is_finite() || cost < 0.0 { return; }
    let line = Line { at: now_ms(), usd: usd(cost), user: user.into(), session: session.chars().take(80).collect() };
    let mut j = json!({ "at": line.at, "usd": line.usd, "user": line.user }); if !line.session.is_empty() { j["session"] = json!(line.session); }
    with_ledger(hub, |l| {
        l.lines.push(line);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(ledger_file(hub)) { let _ = writeln!(f, "{j}"); }
    });
}
/// The start of this calendar month (UTC), in ms.
fn month_start() -> u64 { use chrono::Datelike; let t = chrono::Utc::now().date_naive(); chrono::NaiveDate::from_ymd_opt(t.year(), t.month(), 1).and_then(|d| d.and_hms_opt(0, 0, 0)).map(|d| d.and_utc().timestamp_millis() as u64).unwrap_or(0) }
fn next_month_start() -> u64 { use chrono::Datelike; let t = chrono::Utc::now().date_naive(); let (y, m) = if t.month() == 12 { (t.year() + 1, 1) } else { (t.year(), t.month() + 1) }; chrono::NaiveDate::from_ymd_opt(y, m, 1).and_then(|d| d.and_hms_opt(0, 0, 0)).map(|d| d.and_utc().timestamp_millis() as u64).unwrap_or(0) }

/// Spend in a window that started at `from`, and when a full window frees `over` dollars again (the moment enough of the
/// oldest spend drops out of a rolling window of `len` ms; None for the month, which frees at its turn).
pub fn window(lines: &[Line], now: u64, len: Option<u64>, from: u64, cap: f64) -> (f64, Option<u64>) {
    let mut inside: Vec<&Line> = lines.iter().filter(|l| l.at >= from && l.at <= now).collect();
    inside.sort_by_key(|l| l.at);
    let spent: f64 = inside.iter().map(|l| l.usd).sum();
    let frees = len.and_then(|w| { let mut left = spent; for l in &inside { if left < cap { break; } left -= l.usd; if left < cap { return Some(l.at + w); } } None });
    (usd(spent), frees)
}
/// The three windows now: `[(name, spent, cap, frees_at)]`.
pub fn windows(hub: &dyn AiHub) -> Vec<(&'static str, f64, f64, Option<u64>)> {
    let (c5, c7, cm) = caps(hub); let now = now_ms();
    with_ledger(hub, |l| {
        let (a, af) = window(&l.lines, now, Some(H5), now.saturating_sub(H5), c5);
        let (b, bf) = window(&l.lines, now, Some(D7), now.saturating_sub(D7), c7);
        let (c, _) = window(&l.lines, now, None, month_start(), cm);
        vec![("fiveHour", a, c5, af), ("week", b, c7, bf), ("month", c, cm, Some(next_month_start()))]
    })
}
/// A session's spend on the team key: (USD, answers).
pub fn session_spend(hub: &dyn AiHub, session: &str) -> (f64, u64) {
    if session.is_empty() { return (0.0, 0); }
    with_ledger(hub, |l| { let ls: Vec<&Line> = l.lines.iter().filter(|x| x.session == session).collect(); (usd(ls.iter().map(|x| x.usd).sum()), ls.len() as u64) })
}
fn when(ms: u64) -> String { chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default() }
/// The plain refusal when the team key is at or over one of its limits (None = it may be used).
pub fn refusal(hub: &dyn AiHub) -> Option<String> {
    let names = [("fiveHour", "the last 5 hours"), ("week", "the last 7 days"), ("month", "this month")];
    windows(hub).into_iter().find(|w| w.1 >= w.2).map(|(k, spent, cap, frees)| {
        let what = names.iter().find(|n| n.0 == k).map(|n| n.1).unwrap_or(k);
        format!("The team's OpenRouter limit for {what} is reached: {spent:.2} of {cap:.2} USD{}. An admin can change it in AI Admin → Team OpenRouter.", frees.map(|t| format!("; it frees up {}", if k == "month" { format!("on {}", when(t)) } else { format!("at {}", when(t)) })).unwrap_or_default())
    })
}

// ---------- the key and the account, asked of OpenRouter at most once a minute ----------
static ASKED: LazyLock<Mutex<std::collections::HashMap<String, (Instant, Value)>>> = LazyLock::new(|| Mutex::new(Default::default()));
async fn ask(hub: &dyn AiHub, path: &str, key: &str) -> Result<Value, u16> {
    let ck = format!("{path}:{}", pgm_core_internal::crypto::sha256_hex(key.as_bytes()));
    if let Some((at, v)) = ASKED.lock().unwrap_or_else(|e| e.into_inner()).get(&ck) { if at.elapsed() < ASK_EVERY { return Ok(v.clone()); } }
    let r = crate::openai::self_http(hub).get(format!("{}{path}", crate::openrouter::base_url(hub))).header("authorization", format!("Bearer {key}")).header("HTTP-Referer", hub.public_url()).header("X-Title", "VibeRoller").send();
    let r = tokio::time::timeout(Duration::from_secs(15), r).await.map_err(|_| 504u16)?.map_err(|_| 502u16)?;
    let st = r.status().as_u16(); if !(200..300).contains(&st) { return Err(st); }
    let v: Value = r.json().await.map_err(|_| 502u16)?;
    ASKED.lock().unwrap_or_else(|e| e.into_inner()).insert(ck, (Instant::now(), v["data"].clone()));
    Ok(v["data"].clone())
}
fn team_key(hub: &dyn AiHub) -> String { hub.work().team_key_provider().map(|p| s(&p, "apiKey")).unwrap_or_default() }
/// The key's own limit on OpenRouter (None = unlimited or unknown).
pub async fn key_limit(hub: &dyn AiHub) -> Option<f64> { let k = team_key(hub); if k.is_empty() { return None; } ask(hub, "/key", &k).await.ok().and_then(|d| d["limit"].as_f64()) }

/// Everything the page shows about the team key: the limits and their windows (everyone), and what OpenRouter says about
/// the key and the account; `session` adds that session's spend.
pub async fn status(hub: &dyn AiHub, session: Option<&str>) -> Value {
    let ws = windows(hub);
    let spent: serde_json::Map<String, Value> = ws.iter().map(|(k, s_, c, f)| (k.to_string(), json!({ "spentUsd": s_, "capUsd": c, "over": s_ >= c, "freesAt": f.map(|t| when(t)) }))).collect();
    let (c5, c7, cm) = caps(hub);
    let mut v = json!({ "caps": { "fiveHourUsd": c5, "weekUsd": c7, "monthUsd": cm }, "windows": spent, "over": ws.iter().find(|w| w.1 >= w.2).map(|w| w.0), "hasKey": false, "key": null, "credits": null,
        "hasManagementKey": !s(&settings(hub), "managementKey").is_empty() });
    if let Some(sid) = session { let (u, n) = session_spend(hub, sid); v["session"] = json!({ "id": sid, "spentUsd": u, "answers": n }); }
    let k = team_key(hub);
    if k.is_empty() { return v; }
    v["hasKey"] = json!(true);
    if let Ok(d) = ask(hub, "/key", &k).await {
        v["key"] = json!({ "label": d["label"], "limit": d["limit"], "limitRemaining": d["limit_remaining"], "limitReset": d["limit_reset"], "usage": d["usage"], "usageDaily": d["usage_daily"], "usageWeekly": d["usage_weekly"], "usageMonthly": d["usage_monthly"], "effectiveBudget": d["effective_budget"], "freeTier": d["is_free_tier"] });
    }
    let mk = s(&settings(hub), "managementKey");
    let credits = match ask(hub, "/credits", &k).await { Ok(d) => Some((d, "team key")), Err(_) if !mk.is_empty() => ask(hub, "/credits", &mk).await.ok().map(|d| (d, "management key")), Err(_) => None };
    match credits {
        Some((d, from)) => { let (t, u) = (d["total_credits"].as_f64().unwrap_or(0.0), d["total_usage"].as_f64().unwrap_or(0.0)); v["credits"] = json!({ "totalUsd": usd(t), "usedUsd": usd(u), "leftUsd": usd(t - u), "from": from }); }
        None => { v["creditsWhy"] = json!(if mk.is_empty() { "OpenRouter tells the account's credits to a management key only: add one under Team OpenRouter to see them." } else { "OpenRouter did not tell the account's credits (the management key was refused, or OpenRouter is busy)." }); }
    }
    v
}

fn set_caps(hub: &dyn AiHub, b: &Value, key_limit: Option<f64>) -> Result<(), String> {
    let mut v = settings(hub);
    for (k, words) in [("fiveHourUsd", "5-hour"), ("weekUsd", "weekly"), ("monthUsd", "monthly")] {
        let Some(x) = b[k].as_f64().filter(|x| x.is_finite() && *x >= 0.0 && *x <= 100_000.0) else { return Err(format!("The {words} limit is a number of US dollars from 0.")); };
        if let Some(l) = key_limit.filter(|l| x > *l) { return Err(format!("The {words} limit ({x:.2}) is above the key's own limit on OpenRouter ({l:.2}): the hub cannot spend more than the key may. Raise the key's limit on OpenRouter, or set {l:.2} or less.")); }
        v["caps"][k] = json!(usd(x));
    }
    write_json(&settings_file(hub), &v); Ok(())
}
fn set_management_key(hub: &dyn AiHub, b: &Value) -> Result<(), String> {
    let k = s(b, "apiKey").trim().to_string();
    let mut v = settings(hub);
    if k.is_empty() { if let Some(o) = v.as_object_mut() { o.remove("managementKey"); } } else { if !crate::openrouter::key_ok(&k) { return Err("That does not look like an OpenRouter key (sk-or-…).".into()); } v["managementKey"] = json!(k); }
    write_json(&settings_file(hub), &v); Ok(())
}

/// `GET /api/openrouter/team[?session=]` (signed in: the limits and windows; with Manage AI also the key and the account),
/// `POST /api/openrouter/team/caps {fiveHourUsd, weekUsd, monthUsd}` and `POST /api/openrouter/team/management-key {apiKey}`
/// (Manage AI).
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    if !r.path.starts_with("/api/openrouter/team") { return None; }
    let u = &a.username; let manage = crate::openrouter::manages_ai(&**hub, u);
    let no = || answer(403, json!({ "error": pgm_identity_internal::siteroles::needs(pgm_identity_internal::siteroles::MANAGE_AI) }));
    Some(match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/openrouter/team") => {
            // a session's spend: only the person's own (the session id names it; another's shows nothing)
            let sid = r.param("session").map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).and_then(|x| crate::sessions::resolve(a, &x)).map(|sx| sx.sid.clone());
            let mut v = status(&**hub, sid.as_deref()).await;
            if !manage { if let Some(o) = v.as_object_mut() { o.remove("hasManagementKey"); } } // everyone sees the limits, the windows and what is left
            answer(200, v)
        }
        ("POST", "/api/openrouter/team/caps") => {
            if !manage { return Some(no()); }
            match set_caps(&**hub, &r.json(), key_limit(&**hub).await) {
                Ok(()) => { let (a5, b7, cm) = caps(&**hub); pgm_core_internal::audit::record(u, "openrouter.team.caps", "", json!({ "fiveHourUsd": a5, "weekUsd": b7, "monthUsd": cm }), ""); answer(200, status(&**hub, None).await) }
                Err(e) => answer(400, json!({ "error": e })),
            }
        }
        ("POST", "/api/openrouter/team/management-key") => {
            if !manage { return Some(no()); }
            match set_management_key(&**hub, &r.json()) { Ok(()) => { pgm_core_internal::audit::record(u, "openrouter.team.managementKey", "", json!({ "set": !s(&r.json(), "apiKey").trim().is_empty() }), ""); answer(200, status(&**hub, None).await) } Err(e) => answer(400, json!({ "error": e })) }
        }
        _ => answer(404, json!({ "error": "Not found" })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn l(at: u64, usd: f64) -> Line { Line { at, usd, user: "ann".into(), session: String::new() } }
    #[test]
    fn a_rolling_window_sums_what_is_inside_and_says_when_enough_drops_out() {
        let now = 100 * H5; let from = now - H5;
        let lines = vec![l(from - 1, 50.0), l(from + 10, 4.0), l(from + 20, 3.0), l(now - 5, 4.0)];
        let (spent, frees) = window(&lines, now, Some(H5), from, 10.0);
        assert_eq!(spent, 11.0); // the line before the window does not count
        assert_eq!(frees, Some(from + 10 + H5)); // once the 4.00 from +10 drops out: 7.00 < 10.00
        assert_eq!(window(&lines, now, Some(H5), from, 20.0), (11.0, None)); // under the cap: nothing to wait for
        let (_, f2) = window(&lines, now, Some(H5), from, 5.0); assert_eq!(f2, Some(from + 20 + H5)); // 11 → 7 → 4 < 5
        assert_eq!(window(&[], now, Some(H5), from, 10.0), (0.0, None));
    }
}
