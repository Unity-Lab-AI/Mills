//! Each OpenRouter key's own limits and what is left on its account (T-381, T-383; team notes plans/openrouter-team-admin.md,
//! plans/openrouter-keys-and-jcode-provider.md).
//!
//! **Limits per key, optional** (T-383): a key (a provider of kind `openrouter`, a team one or a person's own) may carry
//! `limits: { fiveHourUsd?, weekUsd?, monthUsd? }`: the last 5 hours (rolling), the last 7 days (rolling, like Claude's
//! 5h / 7d bars in Work) and the calendar month (UTC), for everyone who uses that key together. A limit that is not there
//! is no limit. At or over one, every request on that key is refused before OpenRouter is asked ([`refusal`]), naming the
//! window, the limit and when it frees up; an answer already streaming finishes (that is the only overflow). The key's
//! owner sets them (and anyone holding Manage AI for a team key); when the key itself has a limit on OpenRouter, a limit
//! above it is refused (the hub cannot spend more than the key may), while an unlimited key may get limits above the
//! account's balance (no auto top-up: the balance is the real stop, and the page shows it). A team key's people still
//! have their own month budget on top (openrouter.rs).
//!
//! **The ledger** (`<root>/openrouter-team-ledger.jsonl`, Rust-only, one line per OpenRouter answer: `{ at, usd, user,
//! key?, session? }`, older than 40 days dropped when it is read at start) is kept in memory too; the windows are summed
//! from it per key. A line from before T-383 has no `key`: it counts for the key the team's caps moved to
//! (`legacyKey` in `<root>/openrouter-team.json`, written by the migration in openrouter.rs).
//!
//! **What is left**: `GET /key` (the key's own limit, what it spent today / this week / this month, the budget that blocks
//! it first) and `GET /credits` (the account's credits and use: OpenRouter asks for a management key; the key itself is
//! tried first, then its own optional management key, then the hub-wide one from before T-383). Both are rate-limited per
//! address by OpenRouter, so they are asked at most once a minute per key.
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

/// The limits the team key had before T-383 when nobody had set any (the migration gives them to that key).
pub const DEFAULT_CAPS: (f64, f64, f64) = (10.0, 20.0, 20.0);
const H5: u64 = 5 * 3600 * 1000;
const D7: u64 = 7 * 24 * 3600 * 1000;
const KEEP: u64 = 40 * 24 * 3600 * 1000;
const ASK_EVERY: Duration = Duration::from_secs(60);
pub const LIMIT_NAMES: [(&str, &str, &str); 3] = [("fiveHourUsd", "fiveHour", "5-hour"), ("weekUsd", "week", "weekly"), ("monthUsd", "month", "monthly")];

fn settings_file(hub: &dyn AiHub) -> PathBuf { hub.root().join("openrouter-team.json") }
fn ledger_file(hub: &dyn AiHub) -> PathBuf { hub.root().join("openrouter-team-ledger.jsonl") }
fn settings(hub: &dyn AiHub) -> Value { read_json(&settings_file(hub)).filter(Value::is_object).unwrap_or_else(|| json!({})) }
/// The pre-T-383 team settings, as the migration reads them: the caps (defaults when none were set) and the hub-wide
/// management key.
pub fn old_settings(root: &std::path::Path) -> (Value, String) {
    let v = read_json(&root.join("openrouter-team.json")).filter(Value::is_object).unwrap_or_else(|| json!({}));
    let cap = |k: &str, d: f64| v["caps"][k].as_f64().filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(d);
    (json!({ "fiveHourUsd": cap("fiveHourUsd", DEFAULT_CAPS.0), "weekUsd": cap("weekUsd", DEFAULT_CAPS.1), "monthUsd": cap("monthUsd", DEFAULT_CAPS.2) }), s(&v, "managementKey"))
}
/// The migration names the key the old ledger lines (no `key`) belong to.
pub fn set_legacy_key(root: &std::path::Path, id: &str) {
    let f = root.join("openrouter-team.json");
    let mut v = read_json(&f).filter(Value::is_object).unwrap_or_else(|| json!({}));
    if s(&v, "legacyKey").is_empty() { v["legacyKey"] = json!(id); write_json(&f, &v); }
}
fn legacy_key(hub: &dyn AiHub) -> String { s(&settings(hub), "legacyKey") }
/// A key's limits as (5 h, 7 d, month) in USD; None = no limit.
pub fn limits_of(p: &Value) -> [Option<f64>; 3] { LIMIT_NAMES.map(|(k, _, _)| p["limits"][k].as_f64().filter(|x| x.is_finite() && *x >= 0.0)) }
fn usd(x: f64) -> f64 { (x * 1_000_000.0).round() / 1_000_000.0 }

// ---------- the ledger ----------
#[derive(Clone)]
pub struct Line { pub at: u64, pub usd: f64, pub user: String, pub session: String, pub key: String }
struct Ledger { root: PathBuf, lines: Vec<Line> }
static LEDGER: LazyLock<Mutex<Option<Ledger>>> = LazyLock::new(|| Mutex::new(None));
/// The ledger of this hub, read from its file the first time (and again if the hub's root changed: the suite's hubs).
fn with_ledger<T>(hub: &dyn AiHub, f: impl FnOnce(&mut Ledger) -> T) -> T {
    let mut g = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().map(|l| l.root != hub.root()).unwrap_or(true) {
        let cut = now_ms().saturating_sub(KEEP);
        let lines = std::fs::read_to_string(ledger_file(hub)).unwrap_or_default().lines().filter_map(|t| serde_json::from_str::<Value>(t).ok())
            .map(|v| Line { at: v["at"].as_u64().unwrap_or(0), usd: v["usd"].as_f64().unwrap_or(0.0), user: s(&v, "user"), session: s(&v, "session"), key: s(&v, "key") }).filter(|l| l.at >= cut).collect();
        *g = Some(Ledger { root: hub.root().to_path_buf(), lines });
    }
    f(g.as_mut().expect("just set"))
}
/// One OpenRouter answer's cost goes into the ledger (in memory and on disk), under the key that paid it.
pub fn record(hub: &dyn AiHub, user: &str, cost: f64, session: &str, key: &str) {
    if !cost.is_finite() || cost < 0.0 { return; }
    let line = Line { at: now_ms(), usd: usd(cost), user: user.into(), session: session.chars().take(80).collect(), key: key.chars().take(80).collect() };
    let mut j = json!({ "at": line.at, "usd": line.usd, "user": line.user }); if !line.session.is_empty() { j["session"] = json!(line.session); } if !line.key.is_empty() { j["key"] = json!(line.key); }
    with_ledger(hub, |l| {
        l.lines.push(line);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(ledger_file(hub)) { let _ = writeln!(f, "{j}"); }
    });
}
/// The lines of one key (a line from before T-383 is the legacy key's).
fn of_key(lines: &[Line], id: &str, legacy: &str) -> Vec<Line> { lines.iter().filter(|l| l.key == id || (l.key.is_empty() && !legacy.is_empty() && legacy == id)).cloned().collect() }
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
/// A key's three windows now: `[(name, spent, limit, frees_at)]` (limit None = no limit; frees_at only when it is reached).
pub fn windows(hub: &dyn AiHub, p: &Value) -> Vec<(&'static str, f64, Option<f64>, Option<u64>)> {
    let [c5, c7, cm] = limits_of(p); let now = now_ms(); let (id, legacy) = (s(p, "id"), legacy_key(hub));
    with_ledger(hub, |l| {
        let mine = of_key(&l.lines, &id, &legacy);
        let big = f64::INFINITY;
        let (a, af) = window(&mine, now, Some(H5), now.saturating_sub(H5), c5.unwrap_or(big));
        let (b, bf) = window(&mine, now, Some(D7), now.saturating_sub(D7), c7.unwrap_or(big));
        let (c, _) = window(&mine, now, None, month_start(), cm.unwrap_or(big));
        vec![("fiveHour", a, c5, af), ("week", b, c7, bf), ("month", c, cm, cm.map(|_| next_month_start()))]
    })
}
/// A session's spend on a key (any key when `key` is empty): (USD, answers).
pub fn session_spend(hub: &dyn AiHub, session: &str, key: &str) -> (f64, u64) {
    if session.is_empty() { return (0.0, 0); }
    with_ledger(hub, |l| { let ls: Vec<&Line> = l.lines.iter().filter(|x| x.session == session && (key.is_empty() || x.key == key)).collect(); (usd(ls.iter().map(|x| x.usd).sum()), ls.len() as u64) })
}
fn when(ms: u64) -> String { chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default() }
/// The plain refusal when a key is at or over one of its limits (None = it may be used: no limits, or under them).
pub fn refusal(hub: &dyn AiHub, p: &Value) -> Option<String> {
    if limits_of(p).iter().all(Option::is_none) { return None; }
    let names = [("fiveHour", "the last 5 hours"), ("week", "the last 7 days"), ("month", "this month")];
    windows(hub, p).into_iter().find(|w| w.2.map(|c| w.1 >= c).unwrap_or(false)).map(|(k, spent, cap, frees)| {
        let what = names.iter().find(|n| n.0 == k).map(|n| n.1).unwrap_or(k);
        let who = if crate::openrouter::is_personal(p) { "Its owner can change it in AI Admin → Yours → My OpenRouter keys." } else { "Its owner (or an admin) can change it in AI Admin → Team OpenRouter keys." };
        format!("The OpenRouter key \"{}\" has reached its limit for {what}: {spent:.2} of {:.2} USD{}. {who}", s(p, "label"), cap.unwrap_or(0.0), frees.map(|t| format!("; it frees up {}", if k == "month" { format!("on {}", when(t)) } else { format!("at {}", when(t)) })).unwrap_or_default())
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
/// The key's own limit on OpenRouter (None = unlimited or unknown).
pub async fn key_limit(hub: &dyn AiHub, p: &Value) -> Option<f64> { let k = s(p, "apiKey"); if k.is_empty() { return None; } ask(hub, "/key", &k).await.ok().and_then(|d| d["limit"].as_f64()) }

/// The keys a person sees: every team key, and their own. Full records (the key is in them: never hand one on).
pub fn keys_for(hub: &dyn AiHub, u: &str) -> Vec<Value> {
    let w = hub.work();
    crate::work::providers_list(hub, u).into_iter().filter(|p| p["kind"] == json!(crate::openrouter::KIND)).filter_map(|p| w.provider_get(hub.ai(), &s(&p, "id"), u)).collect()
}
/// Who may change a key: its owner, and for a team key anyone holding Manage AI.
pub fn may_edit(hub: &dyn AiHub, u: &str, p: &Value) -> bool { p["owner"] == json!(u) || (p["shared"] == json!(true) && crate::openrouter::manages_ai(hub, u)) }
/// The key a request names (`key`), else the first team key; only one the person may see.
fn key_named(hub: &dyn AiHub, u: &str, id: &str) -> Option<Value> {
    let ks = keys_for(hub, u);
    if id.is_empty() { ks.iter().find(|p| p["shared"] == json!(true)).or(ks.first()).cloned() } else { ks.into_iter().find(|p| p["id"] == json!(id)) }
}

/// Everything the page shows about one key: its limits and windows (everyone who may use it), and what OpenRouter says
/// about the key and the account; `session` adds that session's spend on it.
pub async fn status(hub: &dyn AiHub, p: &Value, session: Option<&str>) -> Value {
    let ws = windows(hub, p);
    let spent: serde_json::Map<String, Value> = ws.iter().map(|(k, s_, c, f)| (k.to_string(), json!({ "spentUsd": s_, "capUsd": c, "over": c.map(|c| *s_ >= c).unwrap_or(false), "freesAt": f.map(when) }))).collect();
    let [c5, c7, cm] = limits_of(p);
    let mk = { let own = s(p, "managementKey"); if own.is_empty() && s(p, "id") == legacy_key(hub) { s(&settings(hub), "managementKey") } else { own } };
    let mut v = json!({ "id": p["id"], "label": p["label"], "owner": p["owner"], "ownerName": hub.name_of(&s(p, "owner")), "shared": p["shared"] == json!(true), "personal": crate::openrouter::is_personal(p),
        "entries": crate::openrouter::pairs_of(p).len(), "caps": { "fiveHourUsd": c5, "weekUsd": c7, "monthUsd": cm }, "limited": c5.is_some() || c7.is_some() || cm.is_some(), "windows": spent,
        "over": ws.iter().find(|w| w.2.map(|c| w.1 >= c).unwrap_or(false)).map(|w| w.0), "hasKey": false, "key": null, "credits": null, "hasManagementKey": !mk.is_empty() });
    if let Some(sid) = session { let (u, n) = session_spend(hub, sid, &s(p, "id")); v["session"] = json!({ "id": sid, "spentUsd": u, "answers": n }); }
    let k = s(p, "apiKey");
    if k.is_empty() { return v; }
    v["hasKey"] = json!(true); v["last4"] = json!(crate::openrouter::last4(&k));
    if let Ok(d) = ask(hub, "/key", &k).await {
        v["key"] = json!({ "label": d["label"], "limit": d["limit"], "limitRemaining": d["limit_remaining"], "limitReset": d["limit_reset"], "usage": d["usage"], "usageDaily": d["usage_daily"], "usageWeekly": d["usage_weekly"], "usageMonthly": d["usage_monthly"], "effectiveBudget": d["effective_budget"], "freeTier": d["is_free_tier"] });
    }
    let credits = match ask(hub, "/credits", &k).await { Ok(d) => Some((d, "the key")), Err(_) if !mk.is_empty() => ask(hub, "/credits", &mk).await.ok().map(|d| (d, "management key")), Err(_) => None };
    match credits {
        Some((d, from)) => { let (t, u) = (d["total_credits"].as_f64().unwrap_or(0.0), d["total_usage"].as_f64().unwrap_or(0.0)); v["credits"] = json!({ "totalUsd": usd(t), "usedUsd": usd(u), "leftUsd": usd(t - u), "from": from }); }
        None => { v["creditsWhy"] = json!(if mk.is_empty() { "OpenRouter tells the account's credits to a management key only: add one to this key to see them." } else { "OpenRouter did not tell the account's credits (the management key was refused, or OpenRouter is busy)." }); }
    }
    v
}
/// What a person who may not change the key sees of it: no management-key flag.
fn public(mut v: Value, edit: bool) -> Value { v["mayEdit"] = json!(edit); if !edit { if let Some(o) = v.as_object_mut() { o.remove("hasManagementKey"); } } v }

/// A key's new limits from `{ fiveHourUsd, weekUsd, monthUsd }` (each a number from 0, or null / absent = no limit).
pub fn clean_limits(b: &Value, key_limit: Option<f64>) -> Result<Value, String> {
    let mut out = json!({});
    for (k, _, words) in LIMIT_NAMES {
        let x = &b[k];
        if x.is_null() || x.as_str().map(|t| t.trim().is_empty()).unwrap_or(false) { continue; }
        let Some(x) = x.as_f64().or_else(|| x.as_str().and_then(|t| t.trim().parse().ok())).filter(|x: &f64| x.is_finite() && *x >= 0.0 && *x <= 100_000.0) else { return Err(format!("The {words} limit is a number of US dollars from 0, or empty for no limit.")); };
        if let Some(l) = key_limit.filter(|l| x > *l) { return Err(format!("The {words} limit ({x:.2}) is above the key's own limit on OpenRouter ({l:.2}): the hub cannot spend more than the key may. Raise the key's limit on OpenRouter, or set {l:.2} or less.")); }
        out[k] = json!(usd(x));
    }
    Ok(out)
}

/// `GET /api/openrouter/keys[?session=]` (every key the person sees, with its status), `GET /api/openrouter/team[?key=&session=]`
/// (one key: the named one, else the first team key; the shape of T-381), `POST /api/openrouter/team/caps {key?, fiveHourUsd,
/// weekUsd, monthUsd}` (null = no limit) and `POST /api/openrouter/team/management-key {key?, apiKey}` (the key's owner, or
/// Manage AI for a team key).
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    if !r.path.starts_with("/api/openrouter/team") && r.path != "/api/openrouter/keys" { return None; }
    let u = &a.username;
    // a session's spend: only the person's own (the session id names it; another's shows nothing)
    let sid = r.param("session").map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).and_then(|x| crate::sessions::resolve(a, &x)).map(|sx| sx.sid.clone());
    let body = r.json(); let kid = r.param("key").map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).unwrap_or_else(|| s(&body, "key"));
    Some(match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/openrouter/keys") => {
            let mut out = vec![];
            for p in keys_for(&**hub, u) { let e = may_edit(&**hub, u, &p); out.push(public(status(&**hub, &p, sid.as_deref()).await, e)); }
            answer(200, json!({ "keys": out, "manage": crate::openrouter::manages_ai(&**hub, u) }))
        }
        ("GET", "/api/openrouter/team") => match key_named(&**hub, u, &kid) {
            Some(p) => { let e = may_edit(&**hub, u, &p); answer(200, public(status(&**hub, &p, sid.as_deref()).await, e)) }
            None => answer(200, json!({ "hasKey": false, "key": null, "credits": null, "caps": { "fiveHourUsd": null, "weekUsd": null, "monthUsd": null }, "windows": {}, "over": null })),
        },
        ("POST", "/api/openrouter/team/caps") | ("POST", "/api/openrouter/team/management-key") => {
            let Some(p) = key_named(&**hub, u, &kid) else { return Some(answer(404, json!({ "error": "No such OpenRouter key." }))) };
            if !may_edit(&**hub, u, &p) { return Some(answer(403, json!({ "error": "Only the key's owner (or, for a team key, someone with Manage AI) may change it." }))); }
            let id = s(&p, "id");
            let change = if r.path.ends_with("/caps") {
                match clean_limits(&body, key_limit(&**hub, &p).await) { Ok(l) => { pgm_core_internal::audit::record(u, "openrouter.key.limits", &id, l.clone(), ""); crate::openai::edit_provider(&**hub, &id, |x| { x["limits"] = l; }) } Err(e) => return Some(answer(400, json!({ "error": e }))) }
            } else {
                let k = s(&body, "apiKey").trim().to_string();
                if !k.is_empty() && !crate::openrouter::key_ok(&k) { return Some(answer(400, json!({ "error": "That does not look like an OpenRouter key (sk-or-…)." }))); }
                pgm_core_internal::audit::record(u, "openrouter.key.managementKey", &id, json!({ "set": !k.is_empty() }), "");
                crate::openai::edit_provider(&**hub, &id, |x| { if k.is_empty() { if let Some(o) = x.as_object_mut() { o.remove("managementKey"); } } else { x["managementKey"] = json!(k); } })
            };
            if !change { return Some(answer(404, json!({ "error": "No such OpenRouter key." }))); }
            match key_named(&**hub, u, &id) { Some(p) => answer(200, public(status(&**hub, &p, None).await, true)), None => answer(404, json!({ "error": "No such OpenRouter key." })) }
        }
        _ => answer(404, json!({ "error": "Not found" })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn l(at: u64, usd: f64) -> Line { Line { at, usd, user: "ann".into(), session: String::new(), key: String::new() } }
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
        assert_eq!(window(&lines, now, Some(H5), from, f64::INFINITY), (11.0, None)); // no limit: never frees, never over
    }
    #[test]
    fn each_key_counts_its_own_answers_and_the_old_lines_belong_to_the_key_the_team_caps_moved_to() {
        let mk = |key: &str, usd: f64| Line { at: 1, usd, user: "ann".into(), session: String::new(), key: key.into() };
        let lines = vec![mk("p-a", 1.0), mk("p-b", 2.0), mk("", 4.0)];
        let sum = |v: Vec<Line>| v.iter().map(|l| l.usd).sum::<f64>();
        assert_eq!(sum(of_key(&lines, "p-a", "p-a")), 5.0);
        assert_eq!(sum(of_key(&lines, "p-b", "p-a")), 2.0);
        assert_eq!(sum(of_key(&lines, "p-b", "")), 2.0);
    }
    #[test]
    fn limits_are_optional_numbers_and_never_above_the_keys_own() {
        assert_eq!(clean_limits(&json!({}), None).unwrap(), json!({}));
        assert_eq!(clean_limits(&json!({ "fiveHourUsd": null, "weekUsd": "", "monthUsd": 20 }), None).unwrap(), json!({ "monthUsd": 20.0 }));
        assert_eq!(clean_limits(&json!({ "monthUsd": "12.5" }), None).unwrap(), json!({ "monthUsd": 12.5 }));
        assert!(clean_limits(&json!({ "monthUsd": -1 }), None).is_err());
        assert!(clean_limits(&json!({ "monthUsd": "lots" }), None).is_err());
        assert!(clean_limits(&json!({ "monthUsd": 50 }), Some(20.0)).unwrap_err().contains("above the key's own limit"));
        assert_eq!(limits_of(&json!({ "limits": { "monthUsd": 20 } })), [None, None, Some(20.0)]);
        assert_eq!(limits_of(&json!({})), [None, None, None]);
    }
}
