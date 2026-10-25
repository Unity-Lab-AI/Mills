//! Time accounting and the activity log (plans/presence.md section 6, T-263): per person per day, kept in
//! `profiles/<user>/activity/<YYYY-MM-DD>.json` = `{activeMs, awayMs, aiMs, agentMs, spans: [[start, end]], events, updatedAt}`.
//! New files only (the JavaScript hub never reads them).
//! - **Active time** is the sum of spans of activity: an activity within `workGapMs` (default 15 min) of the last one
//!   extends the span; after a longer quiet the span ends at the last activity (the quiet is not counted) and the next
//!   activity starts a new span.
//! - **AI time** keeps adding while the person is away or offline: `aiMs` = wall-clock time with any session working,
//!   `agentMs` = summed over sessions.
//! - **Away time** = a device live while outside an active span (counted once the gap has passed).
//! - **events** = the append-only log of the day: status (label) changes and how many sessions work (capped at 500 a day).
use pgm_core_internal::json::{read_json, write_json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_GAP_MS: u64 = 900_000;
const MAX_EVENTS: usize = 500;
const SAVE_MS: u64 = 5000;
const MAX_DT: u64 = 5000;

/// The day (YYYY-MM-DD, the hub's time zone) a time falls on.
pub fn day_of(ms: u64) -> String { chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string()).unwrap_or_default() }

#[derive(Default, Clone)]
struct Rec { spans: Vec<[u64; 2]>, away: u64, ai: u64, agent: u64, events: Vec<Value> }
impl Rec {
    fn active(&self) -> u64 { self.spans.iter().map(|s| s[1].saturating_sub(s[0])).sum() }
    fn from_json(v: &Value) -> Rec {
        Rec { spans: v["spans"].as_array().map(|a| a.iter().filter_map(|s| Some([s[0].as_u64()?, s[1].as_u64()?])).collect()).unwrap_or_default(),
            away: v["awayMs"].as_u64().unwrap_or(0), ai: v["aiMs"].as_u64().unwrap_or(0), agent: v["agentMs"].as_u64().unwrap_or(0), events: v["events"].as_array().cloned().unwrap_or_default() }
    }
    fn to_json(&self, now: u64) -> Value { json!({ "activeMs": self.active(), "awayMs": self.away, "aiMs": self.ai, "agentMs": self.agent, "spans": self.spans, "events": self.events, "updatedAt": now }) }
    fn event(&mut self, e: Value) { self.events.push(e); if self.events.len() > MAX_EVENTS { let n = self.events.len() - MAX_EVENTS; self.events.drain(..n); } }
}
struct U { day: String, rec: Rec, dir: PathBuf, dirty: bool, saved: u64, last_tick: u64, sessions: u64, label: String }
#[derive(Default)]
pub struct Tracker { m: HashMap<String, U> }

fn file(dir: &Path, day: &str) -> PathBuf { dir.join("activity").join(format!("{day}.json")) }
impl U {
    fn save(&mut self, now: u64) {
        if self.day.is_empty() { return; }
        let f = file(&self.dir, &self.day); if let Some(p) = f.parent() { let _ = std::fs::create_dir_all(p); }
        write_json(&f, &self.rec.to_json(now)); self.dirty = false; self.saved = now;
    }
}
impl Tracker {
    /// The person's record for the day `now` falls on (a new day saves the old one and loads the new).
    fn cur(&mut self, u: &str, dir: &Path, now: u64) -> &mut U {
        let e = self.m.entry(u.to_string()).or_insert_with(|| U { day: String::new(), rec: Rec::default(), dir: dir.to_path_buf(), dirty: false, saved: 0, last_tick: 0, sessions: 0, label: String::new() });
        e.dir = dir.to_path_buf();
        let day = day_of(now);
        if e.day != day { if e.dirty { e.save(now); } e.rec = read_json(&file(dir, &day)).map(|v| Rec::from_json(&v)).unwrap_or_default(); e.day = day; }
        e
    }
    /// The person did something (input on a device, a message, ...): extend the span, or start a new one after a quiet longer than `gap`.
    pub fn touch(&mut self, u: &str, dir: &Path, now: u64, gap: u64) {
        let e = self.cur(u, dir, now);
        match e.rec.spans.last_mut() { Some(s) if now >= s[1] && now - s[1] <= gap => s[1] = now, _ => e.rec.spans.push([now, now]) }
        e.dirty = true;
    }
    /// Once a second per person: `live` = a device is online or away, `sessions` = how many of their sessions work, `label` = their status label.
    pub fn tick(&mut self, u: &str, dir: &Path, now: u64, gap: u64, live: bool, sessions: u64, label: &str) {
        let e = self.cur(u, dir, now);
        if e.last_tick == 0 { e.last_tick = now; e.sessions = sessions; e.label = label.to_string(); return; }
        let dt = now.saturating_sub(e.last_tick).min(MAX_DT); e.last_tick = now;
        if sessions > 0 && dt > 0 { e.rec.ai += dt; e.rec.agent += dt * sessions; e.dirty = true; }
        let in_span = e.rec.spans.last().map(|s| now.saturating_sub(s[1]) <= gap).unwrap_or(false);
        if live && !in_span && dt > 0 { e.rec.away += dt; e.dirty = true; }
        if label != e.label { e.rec.event(json!({ "at": now, "kind": "status", "label": label, "was": e.label })); e.label = label.to_string(); e.dirty = true; }
        if sessions != e.sessions { e.rec.event(json!({ "at": now, "kind": "ai", "sessions": sessions, "was": e.sessions })); e.sessions = sessions; e.dirty = true; }
        if e.dirty && now.saturating_sub(e.saved) >= SAVE_MS { e.save(now); }
    }
    /// Everything not written yet goes to disk (before a read).
    pub fn flush(&mut self, now: u64) { for e in self.m.values_mut() { if e.dirty { e.save(now); } } }
}

/// The days of `from..=to` (YYYY-MM-DD, at most 366) of one person, read from their files; `own` keeps spans and events.
pub fn days(dir: &Path, from: &str, to: &str, own: bool) -> Result<Vec<Value>, String> {
    let p = |x: &str| chrono::NaiveDate::parse_from_str(x, "%Y-%m-%d").map_err(|_| format!("'{x}' is not a day (YYYY-MM-DD)."));
    let (mut d, end) = (p(from)?, p(to)?);
    if end < d || (end - d).num_days() > 365 { return Err("from..to must be 1 to 366 days.".into()); }
    let mut out = vec![];
    loop {
        let day = d.format("%Y-%m-%d").to_string();
        if let Some(mut v) = read_json(&file(dir, &day)) {
            if !own { if let Some(o) = v.as_object_mut() { o.remove("spans"); o.remove("events"); } }
            v["day"] = json!(day); out.push(v);
        }
        match d.succ_opt() { Some(n) if n <= end => d = n, _ => break }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gap_is_not_counted_and_ai_keeps_adding() {
        let dir = std::env::temp_dir().join(format!("vr-act-{}", pgm_core_internal::now_ms())); let _ = std::fs::create_dir_all(&dir);
        let mut t = Tracker::default(); let b = 1_800_000_000_000u64;
        t.touch("u", &dir, b, 1000); t.touch("u", &dir, b + 800, 1000); t.touch("u", &dir, b + 5000, 1000); t.touch("u", &dir, b + 5500, 1000);
        assert_eq!(t.m["u"].rec.spans, vec![[b, b + 800], [b + 5000, b + 5500]]); assert_eq!(t.m["u"].rec.active(), 1300);
        t.tick("u", &dir, b + 6000, 1000, true, 0, "Online"); t.tick("u", &dir, b + 9000, 1000, true, 2, "Working");
        let r = &t.m["u"].rec; assert_eq!((r.ai, r.agent, r.away), (3000, 6000, 3000)); assert_eq!(r.events.len(), 2);
        t.flush(b + 9000); let v = days(&dir, &day_of(b), &day_of(b), true).unwrap(); assert_eq!(v[0]["activeMs"], json!(1300));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
