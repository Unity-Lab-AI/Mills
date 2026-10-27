//! The team board (board T-131, module "board"), moved from bridge/board.js: tasks that people and Claude agents work
//! at the same time. One task is both the board card and the calendar entry; advisory, time-bounded claims (nobody
//! else edits a task while it is held; an unrenewed claim lapses); a revision per task (an update based on an older one
//! is refused); deletes are tombstones; every change goes to the activity log; month-end aware repeats.
//! The archive (T-190): a done task gets `archivedAt` (by hand, "archive all done", or the sweep after ARCHIVE_AFTER_MS)
//! and leaves the board and the calendar; it stays readable and restorable until the sweep deletes it (DELETE_AFTER_MS).
//! Add-only fields (`archivedAt`, `doneAt`): the JavaScript hub reads the same file and shows such a task as done.
//! Storage: VibeRoller/Board/board.json (the same file and shape the JavaScript module used), written atomically.
//! People: GET /api/board (long poll ?wait=1&ver=N), /api/board/{create,task,update,move,complete,claim,release,delete,new,
//! archive,restore,archive-done,archived}.
//! Claude: the board MCP tools, handed over by the JavaScript hub after it checked the run's token (/__front/board/tool).
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use pgm_core_internal::now_ms;

pub const STATUS_IDS: [&str; 5] = ["backlog", "todo", "doing", "review", "done"];
const RECUR: [&str; 7] = ["daily", "weekly", "biweekly", "monthly", "quarterly", "half_year", "yearly"];
const CLAIM_DEFAULT_MIN: f64 = 15.0;
const CLAIM_MAX_MIN: f64 = 240.0;
const ACTIVITY_KEEP: usize = 3000;
const DAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
/// The archive (T-190): a done task is archived after this long, an archived one deleted (a tombstone) after this long.
pub const ARCHIVE_AFTER_MS: i64 = 48 * 3_600_000;
pub const DELETE_AFTER_MS: i64 = 30 * 86_400_000;

fn statuses() -> Value { json!([{ "id": "backlog", "label": "Backlog" }, { "id": "todo", "label": "To do" }, { "id": "doing", "label": "Doing" }, { "id": "review", "label": "Review" }, { "id": "done", "label": "Done" }]) }

// ---------- dates, in this machine's local time (as the JavaScript module did) ----------
fn local(ms: i64) -> chrono::DateTime<Local> { Local.timestamp_millis_opt(ms).single().unwrap_or_else(Local::now) }
fn at(y: i32, m: u32, d: u32, h: u32, mi: u32) -> Option<i64> {
    // JavaScript's new Date(y, m, d, …) rolls over (month 13 = next January, day 0 = the last day before): the same here
    let mut y = y; let mut m = m as i32;
    while m > 12 { m -= 12; y += 1; } while m < 1 { m += 12; y -= 1; }
    let base = NaiveDate::from_ymd_opt(y, m as u32, 1)?;
    let date = base.checked_add_signed(chrono::Duration::days(d as i64 - 1))?;
    let dt = date.and_hms_opt(h, mi, 0)?;
    Local.from_local_datetime(&dt).earliest().map(|t| t.timestamp_millis())
}
fn at17_days_from_today(days: i64) -> i64 { let t = Local::now().date_naive() + chrono::Duration::days(days); at(t.year(), t.month(), t.day(), 17, 0).unwrap_or(0) }
fn last_day(y: i32, m: u32) -> u32 { let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) }; (NaiveDate::from_ymd_opt(ny, nm, 1).unwrap() - chrono::Duration::days(1)).day() }
fn add_months_clamped(ms: i64, months: i32, anchor: u32) -> i64 {
    let d = local(ms);
    let day = if anchor > 0 { anchor } else { d.day() };
    let total = d.month0() as i32 + months; let y = d.year() + total.div_euclid(12); let m = total.rem_euclid(12) as u32 + 1;
    let dd = day.min(last_day(y, m));
    let dt = NaiveDate::from_ymd_opt(y, m, dd).and_then(|x| x.and_hms_opt(d.hour(), d.minute(), d.second()));
    dt.and_then(|x| Local.from_local_datetime(&x).earliest()).map(|t| t.timestamp_millis()).unwrap_or(ms)
}
pub fn next_due(ms: i64, kind: &str, anchor: u32) -> Option<i64> {
    const DAY: i64 = 86_400_000;
    Some(match kind { "daily" => ms + DAY, "weekly" => ms + 7 * DAY, "biweekly" => ms + 14 * DAY, "monthly" => add_months_clamped(ms, 1, anchor), "quarterly" => add_months_clamped(ms, 3, anchor), "half_year" => add_months_clamped(ms, 6, anchor), "yearly" => add_months_clamped(ms, 12, anchor), _ => return None })
}
/// Due dates as people (and Claude) write them → ms at 17:00 local, or None for "none". A typo is an error.
pub fn parse_due(v: &Value) -> Result<Option<i64>, String> {
    match v {
        Value::Null => return Ok(None),
        Value::Number(n) => { let x = n.as_f64().unwrap_or(0.0); return Ok(if x.is_finite() && x > 0.0 { Some(x as i64) } else { None }); }
        _ => {}
    }
    let raw = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
    let t = raw.trim().to_lowercase();
    if t.is_empty() || ["none", "clear", "no", "null"].contains(&t.as_str()) { return Ok(None); }
    if t.len() >= 12 && t.bytes().all(|b| b.is_ascii_digit()) { return Ok(t.parse().ok()); }
    if t == "today" { return Ok(Some(at17_days_from_today(0))); }
    if t == "tomorrow" { return Ok(Some(at17_days_from_today(1))); }
    // +3d / in 3 days / 2w / in 2 weeks
    let rel = t.strip_prefix('+').or_else(|| t.strip_prefix("in ")).unwrap_or(&t);
    let digits: String = rel.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        let unit = rel[digits.len()..].trim_start();
        if ["d", "day", "days"].contains(&unit) { return Ok(Some(at17_days_from_today(digits.parse::<i64>().unwrap_or(0)))); }
        if ["w", "week", "weeks"].contains(&unit) { return Ok(Some(at17_days_from_today(7 * digits.parse::<i64>().unwrap_or(0)))); }
    }
    let wd = t.strip_prefix("next ").unwrap_or(&t);
    if let Some(want) = DAYS.iter().position(|d| *d == wd) {
        let today = Local::now().weekday().num_days_from_sunday() as i64;
        let mut d = (want as i64 - today + 7) % 7; if d == 0 { d = 7; }
        return Ok(Some(at17_days_from_today(d)));
    }
    // YYYY-MM-DD[ HH:MM]
    let b = t.as_bytes();
    if t.len() >= 10 && b[4] == b'-' && b[7] == b'-' && t[..4].bytes().all(|c| c.is_ascii_digit()) && t[5..7].bytes().all(|c| c.is_ascii_digit()) && t[8..10].bytes().all(|c| c.is_ascii_digit()) {
        let rest = &t[10..];
        let (h, mi) = if rest.is_empty() { (17, 0) } else if (rest.starts_with(' ') || rest.starts_with('t')) && rest[1..].contains(':') {
            let (hh, mm) = rest[1..].split_once(':').unwrap_or(("x", "x"));
            match (hh.parse::<u32>(), mm.parse::<u32>()) { (Ok(h), Ok(m)) if hh.len() <= 2 && mm.len() == 2 => (h, m), _ => return Err(format!("Could not read the due date \"{raw}\" (use YYYY-MM-DD, today, tomorrow, +3d, next friday, or none).")) }
        } else { return Err(format!("Could not read the due date \"{raw}\" (use YYYY-MM-DD, today, tomorrow, +3d, next friday, or none).")) };
        return at(t[..4].parse().unwrap_or(0), t[5..7].parse().unwrap_or(0), t[8..10].parse().unwrap_or(0), h, mi).map(Some).ok_or_else(|| format!("Bad date: {raw}"));
    }
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(&raw.trim().to_uppercase()) { return Ok(Some(d.timestamp_millis())); }
    if let Ok(d) = chrono::DateTime::parse_from_rfc2822(raw.trim()) { return Ok(Some(d.timestamp_millis())); }
    Err(format!("Could not read the due date \"{raw}\" (use YYYY-MM-DD, today, tomorrow, +3d, next friday, or none)."))
}
pub fn day_key(ms: i64) -> String { let d = local(ms); format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day()) }
fn time_of(ms: i64) -> String { local(ms).format("%H:%M:%S").to_string() }
fn iso_minute(ms: i64) -> String { chrono::Utc.timestamp_millis_opt(ms).single().map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default() }

/// Who acts: a person, or their Claude (agent).
#[derive(Clone, Debug)]
pub struct Actor { pub user: String, pub agent: bool, pub name: String }
impl Actor {
    pub fn key(&self) -> String { if self.agent { format!("{}#agent", self.user) } else { self.user.clone() } }
    pub fn label(&self) -> String { if self.agent { format!("{} · Claude", self.name) } else { self.name.clone() } }
}
/// A change the Forgejo mirror follows: (what, the task as it is now, who made it).
pub type Change = (String, Value, Option<Actor>);
pub struct Out { pub status: u16, pub body: Value, pub changes: Vec<Change> }
fn ok(body: Value) -> Out { Out { status: 200, body, changes: vec![] } }
fn no(status: u16, e: impl Into<String>) -> Out { Out { status, body: json!({ "error": e.into() }), changes: vec![] } }

pub struct Board {
    file: PathBuf,
    db: Mutex<Value>,
    pub ver: tokio::sync::watch::Sender<u64>,
    viewers: Mutex<HashMap<String, i64>>,
    /// (archive a done task after, delete an archived task after) in ms; 0 = never
    retention: Mutex<(i64, i64)>,
}
impl Board {
    pub fn new(dir: PathBuf) -> Board {
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("board.json");
        let mut db: Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        if !db["tasks"].is_array() { db = json!({ "n": 0, "boards": [{ "id": "team", "name": "Team" }], "tasks": [], "activity": [] }); }
        if !db["activity"].is_array() { db["activity"] = json!([]); }
        if !db["boards"].is_array() || db["boards"].as_array().map(|a| a.is_empty()).unwrap_or(true) { db["boards"] = json!([{ "id": "team", "name": "Team" }]); }
        let (ver, _) = tokio::sync::watch::channel(1u64);
        Board { file, db: Mutex::new(db), ver, viewers: Mutex::default(), retention: Mutex::new((ARCHIVE_AFTER_MS, DELETE_AFTER_MS)) }
    }
    /// How long a done task stays before the sweep archives it, and an archived one before it is deleted (ms, 0 = never).
    pub fn set_retention(&self, archive_after_ms: i64, delete_after_ms: i64) { *self.retention.lock().unwrap_or_else(|e| e.into_inner()) = (archive_after_ms.max(0), delete_after_ms.max(0)); }
    pub fn retention(&self) -> (i64, i64) { *self.retention.lock().unwrap_or_else(|e| e.into_inner()) }
    /// The archive's sweep at `now` (T-190): done tasks finished longer ago than the archive time are archived (never a
    /// repeating one, one due in the future, or one somebody holds); tasks archived longer ago than the delete time are
    /// deleted (the usual tombstone). Returns (archived ids, deleted ids). Signed "Board" in the activity log.
    pub fn sweep(&self, now: i64) -> (Vec<String>, Vec<String>) {
        let (arch_after, del_after) = self.retention();
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut archived = vec![]; let mut deleted = vec![];
        for t in db["tasks"].as_array_mut().into_iter().flatten().filter(|t| live(t)) {
            if is_archived(t) {
                if del_after > 0 && t["archivedAt"].as_i64().unwrap_or(now) <= now - del_after { t["deleted"] = json!(true); t["claim"] = Value::Null; touch(t, now); deleted.push(t.clone()); }
            } else if arch_after > 0 && t["status"] == json!("done") && t["recur"].as_str().is_none() && !t["dueAt"].as_i64().map(|d| d > now).unwrap_or(false) && !claim_active(t, now)
                && done_since(t) <= now - arch_after {
                t["archivedAt"] = json!(now); t["claim"] = Value::Null; touch(t, now); archived.push(t.clone());
            }
        }
        if archived.is_empty() && deleted.is_empty() { return (vec![], vec![]); }
        let sys = Actor { user: "board".into(), agent: false, name: "Board".into() };
        for t in &archived { log(&mut db, Some(t), &sys, "archived", Some(format!("done for over {} h", arch_after / 3_600_000))); }
        for t in &deleted { log(&mut db, Some(t), &sys, "deleted", Some(format!("archived for over {} days", del_after / 86_400_000))); }
        self.save(&mut db);
        let ids = |v: &Vec<Value>| v.iter().map(|t| t["id"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>();
        (ids(&archived), ids(&deleted))
    }
    fn save(&self, db: &mut Value) {
        if let Some(a) = db["activity"].as_array_mut() { if a.len() > ACTIVITY_KEEP { let cut = a.len() - ACTIVITY_KEEP; a.drain(0..cut); } }
        let tmp = self.file.with_extension("json.tmp");
        if std::fs::write(&tmp, db.to_string()).is_ok() { let _ = std::fs::rename(&tmp, &self.file); }
        self.ver.send_modify(|v| *v += 1);
    }
    pub fn version(&self) -> u64 { *self.ver.borrow() }
    /// The task this person holds a claim on (their own or their Claude's): the most recently claimed live one, as
    /// (id, title, claimed at ms). Presence shows it (T-260).
    pub fn held_by(&self, user: &str) -> Option<(String, String, u64)> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner()); let now = now_ms() as i64;
        let at = |t: &Value| t["claim"]["at"].as_i64().unwrap_or(t["updatedAt"].as_i64().unwrap_or(0));
        db["tasks"].as_array()?.iter().filter(|t| live(t) && claim_active(t, now) && t["claim"]["user"] == json!(user)).max_by_key(|t| at(t))
            .map(|t| (t["id"].as_str().unwrap_or("").to_string(), t["title"].as_str().unwrap_or("").to_string(), at(t).max(0) as u64))
    }
    pub fn seen(&self, user: &str) { self.viewers.lock().unwrap_or_else(|e| e.into_inner()).insert(user.to_string(), now_ms() as i64); }
    /// A board with that id exists after this (made with `name` if it did not; an existing one is renamed to `name`):
    /// a program whose boards stand for something else (a chat's servers) keeps them in step this way.
    pub fn ensure_board(&self, id: &str, name: &str) {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let changed = match db["boards"].as_array_mut().unwrap().iter_mut().find(|x| x["id"] == json!(id)) {
            Some(x) => { if x["name"] != json!(name) { x["name"] = json!(name); true } else { false } }
            None => { db["boards"].as_array_mut().unwrap().push(json!({ "id": id, "name": name })); true }
        };
        if changed { self.save(&mut db); }
    }
    /// Which board a live task is on.
    pub fn board_of(&self, task_id: &str) -> Option<String> { let db = self.db.lock().unwrap_or_else(|e| e.into_inner()); find(&db, task_id).and_then(|t| t["board"].as_str().map(String::from)) }

    /// The board as a page shows it.
    pub fn view(&self, user: &str, name_of: &dyn Fn(&str) -> String, has: &dyn Fn(&str) -> bool, mirror: Value) -> Value {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_ms() as i64;
        let tasks: Vec<&Value> = db["tasks"].as_array().map(|a| a.iter().filter(|t| shown(t)).collect()).unwrap_or_default();
        let mut presence: Vec<Value> = vec![];
        for (u, t) in self.viewers.lock().unwrap_or_else(|e| e.into_inner()).iter() { if now - t < 60_000 && has(u) { presence.push(json!({ "user": u, "name": name_of(u), "agent": false })); } }
        for t in &tasks { if claim_active(t, now) && t["claim"]["agent"].as_bool() == Some(true) && !presence.iter().any(|p| p["key"] == t["claim"]["key"]) { presence.push(json!({ "key": t["claim"]["key"], "name": t["claim"]["by"], "agent": true })); } }
        let act: Vec<Value> = db["activity"].as_array().map(|a| a.iter().rev().take(60).cloned().collect()).unwrap_or_default();
        let archived = db["tasks"].as_array().map(|a| a.iter().filter(|t| live(t) && is_archived(t)).count()).unwrap_or(0);
        json!({ "ver": self.version(), "statuses": statuses(), "boards": db["boards"], "tasks": tasks.iter().map(|t| card(t, now)).collect::<Vec<_>>(), "presence": presence, "activity": act, "me": user, "mirror": mirror,
            "archive": self.archive_info(archived) })
    }

    /// What a page needs to know about the archive: how many tasks are in it, and the two times (0 = never).
    fn archive_info(&self, count: usize) -> Value {
        let (a, d) = self.retention();
        json!({ "count": count, "archiveAfterHours": a as f64 / 3_600_000.0, "deleteAfterDays": d as f64 / 86_400_000.0 })
    }

    // ---------- the operations shared by people and Claude ----------
    pub fn op(&self, op: &str, a: &Actor, b: &Value, has: &dyn Fn(&str) -> bool, name_of: &dyn Fn(&str) -> String) -> Out {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let out = self.op_locked(&mut db, op, a, b, has, name_of);
        out
    }
    fn op_locked(&self, db: &mut Value, op: &str, a: &Actor, b: &Value, has: &dyn Fn(&str) -> bool, name_of: &dyn Fn(&str) -> String) -> Out {
        let now = now_ms() as i64;
        let skip = b["skipMirror"].as_bool() == Some(true);
        let change = |what: &str, t: &Value| -> Vec<Change> { if skip { vec![] } else { vec![(what.to_string(), t.clone(), Some(a.clone()))] } };
        let id = b["id"].as_str().unwrap_or("").to_string();
        match op {
            "create" => {
                let title: String = b["title"].as_str().map(|s| s.trim().chars().take(200).collect()).unwrap_or_default();
                if title.is_empty() { return no(400, "A task needs a title."); }
                let boards = db["boards"].as_array().cloned().unwrap_or_default();
                let board = if boards.iter().any(|x| x["id"] == b["board"]) { b["board"].as_str().unwrap_or("team").to_string() } else { boards[0]["id"].as_str().unwrap_or("team").to_string() };
                let status = b["status"].as_str().filter(|s| STATUS_IDS.contains(s)).unwrap_or("todo").to_string();
                let due = if !b["due"].is_null() || b.get("due").is_some() { match parse_due(&b["due"]) { Ok(d) => d, Err(e) => return no(400, e) } } else { b["dueAt"].as_f64().filter(|x| *x > 0.0).map(|x| x as i64) };
                let order = 1 + db["tasks"].as_array().map(|ts| ts.iter().filter(|t| live(t) && t["board"] == json!(board) && t["status"] == json!(status)).map(|t| t["order"].as_i64().unwrap_or(0)).max().unwrap_or(0).max(0)).unwrap_or(0);
                let n = db["n"].as_i64().unwrap_or(0) + 1; db["n"] = json!(n);
                let assignee = b["assignee"].as_str().filter(|u| has(u)).map(String::from);
                let mut t = json!({ "id": format!("T-{n}"), "board": board, "title": title, "body": b["body"].as_str().map(|s| s.chars().take(50000).collect::<String>()).unwrap_or_default(), "status": status, "order": order,
                    "priority": clamp_pri(&b["priority"]), "labels": clean_labels(&b["labels"]), "dueAt": due, "recur": b["recur"].as_str().filter(|r| RECUR.contains(r)),
                    "assignee": assignee, "createdBy": a.label(), "createdAt": now, "updatedAt": now, "rev": 1, "claim": Value::Null });
                t["dueDay"] = due.map(|d| json!(local(d).day())).unwrap_or(Value::Null);
                db["tasks"].as_array_mut().unwrap().push(t.clone());
                log(db, Some(&t), a, "created", None); self.save(db);
                let mut task = card(&t, now); task["body"] = t["body"].clone();
                Out { status: 200, body: json!({ "task": task }), changes: change("created", &t) }
            }
            "get" => {
                let Some(t) = find(db, &id) else { return no(404, "No such task.") };
                let mut task = card(t, now); task["body"] = json!(t["body"].as_str().unwrap_or(""));
                let act: Vec<Value> = db["activity"].as_array().map(|ac| ac.iter().filter(|x| x["task"] == json!(id)).cloned().collect::<Vec<_>>()).map(|v| v.into_iter().rev().take(40).collect()).unwrap_or_default();
                ok(json!({ "task": task, "activity": act }))
            }
            "update" => {
                let rev = b.get("rev").filter(|v| !v.is_null()).cloned();
                if let Some(g) = guard(find(db, &id), a, rev.as_ref(), now) { return g; }
                let mut changed: Vec<String> = vec![];
                let mut to_done = false;
                {
                    let t = find_mut(db, &id).unwrap();
                    if let Some(v) = b["title"].as_str() { let v: String = v.trim().chars().take(200).collect(); if !v.is_empty() && json!(v) != t["title"] { t["title"] = json!(v); changed.push("title".into()); } }
                    if let Some(v) = b.get("body").filter(|v| !v.is_null()) { let v: String = v.as_str().map(String::from).unwrap_or_else(|| v.to_string()).chars().take(50000).collect(); if v != t["body"].as_str().unwrap_or("") { t["body"] = json!(v); changed.push("description".into()); } }
                    if let Some(n) = b["appendNote"].as_str().filter(|s| !s.is_empty()) {
                        let note: String = n.chars().take(8000).collect();
                        t["body"] = json!(format!("{}\n\n---\n**{}** · {}\n\n{}", t["body"].as_str().unwrap_or(""), a.label(), iso_minute(now), note).trim().to_string());
                        changed.push("note".into());
                    }
                    if b.get("labels").is_some() { t["labels"] = clean_labels(&b["labels"]); changed.push("labels".into()); }
                    if b.get("priority").is_some() { t["priority"] = json!(clamp_pri(&b["priority"])); changed.push(format!("priority {}", t["priority"])); }
                    if b.get("due").is_some() || b.get("dueAt").is_some() {
                        let due = if b.get("due").is_some() { match parse_due(&b["due"]) { Ok(d) => d, Err(e) => return no(400, e) } } else { b["dueAt"].as_f64().filter(|x| *x > 0.0).map(|x| x as i64) };
                        t["dueAt"] = json!(due); t["dueDay"] = due.map(|d| json!(local(d).day())).unwrap_or(Value::Null);
                        changed.push(due.map(|d| format!("due {}", day_key(d))).unwrap_or_else(|| "no due date".into()));
                    }
                    if b.get("recur").is_some() { let r = b["recur"].as_str().filter(|r| RECUR.contains(r)); t["recur"] = json!(r); changed.push(r.map(|r| format!("repeats {r}")).unwrap_or_else(|| "no repeat".into())); }
                    if b.get("assignee").is_some() { let asg = b["assignee"].as_str().filter(|u| !u.is_empty() && has(u)).map(String::from); changed.push(asg.as_ref().map(|u| format!("assigned to {}", name_of(u))).unwrap_or_else(|| "unassigned".into())); t["assignee"] = json!(asg); }
                    if let Some(s) = b["status"].as_str().filter(|s| STATUS_IDS.contains(s)) { if json!(s) != t["status"] { if s == "done" { to_done = true; } else { t["status"] = json!(s); changed.push(format!("moved to {s}")); if unarchive(t) { changed.push("restored from the archive".into()); } } } }
                }
                if to_done { return self.op_locked(db, "complete", a, &json!({ "id": id, "skipMirror": skip }), has, name_of); }
                let t = find_mut(db, &id).unwrap();
                if changed.is_empty() { let mut task = card(t, now); task["body"] = json!(t["body"].as_str().unwrap_or("")); return ok(json!({ "task": task })); }
                touch(t, now);
                let tc = t.clone();
                log(db, Some(&tc), a, "updated", Some(changed.join(", "))); self.save(db);
                let mut task = card(&tc, now); task["body"] = json!(tc["body"].as_str().unwrap_or(""));
                Out { status: 200, body: json!({ "task": task }), changes: change("updated", &tc) }
            }
            "move" => {
                if let Some(g) = guard(find(db, &id), a, None, now) { return g; }
                let cur = find(db, &id).unwrap().clone();
                let status = b["status"].as_str().filter(|s| STATUS_IDS.contains(s)).unwrap_or(cur["status"].as_str().unwrap_or("todo")).to_string();
                if status == "done" && cur["status"] != json!("done") { return self.op_locked(db, "complete", a, &json!({ "id": id }), has, name_of); }
                let mut col: Vec<(String, i64)> = db["tasks"].as_array().unwrap().iter().filter(|x| live(x) && x["board"] == cur["board"] && x["status"] == json!(status) && x["id"] != cur["id"]).map(|x| (x["id"].as_str().unwrap_or("").to_string(), x["order"].as_i64().unwrap_or(0))).collect();
                col.sort_by_key(|x| x.1);
                let idx = (b["index"].as_i64().unwrap_or(0).max(0) as usize).min(col.len());
                col.insert(idx, (id.clone(), 0));
                for (i, (cid, _)) in col.iter().enumerate() { if let Some(x) = find_mut(db, cid) { x["order"] = json!(i + 1); } }
                let moved = cur["status"] != json!(status);
                let t = find_mut(db, &id).unwrap();
                t["status"] = json!(status); touch(t, now);
                let restored = moved && unarchive(t);
                let tc = t.clone();
                if moved { log(db, Some(&tc), a, "moved", Some(format!("to {status}{}", if restored { ", restored from the archive" } else { "" }))); }
                self.save(db);
                Out { status: 200, body: json!({ "task": card(&tc, now) }), changes: if moved { change("updated", &tc) } else { vec![] } }
            }
            "complete" => {
                if let Some(g) = guard(find(db, &id), a, None, now) { return g; }
                let t = find_mut(db, &id).unwrap();
                let detail;
                if let (Some(r), Some(due)) = (t["recur"].as_str().map(String::from), t["dueAt"].as_i64()) { // a repeating task re-arms instead of finishing
                    let anchor = t["dueDay"].as_u64().map(|d| d as u32).unwrap_or_else(|| local(due).day());
                    let next = next_due(due, &r, anchor).unwrap_or(due);
                    t["dueAt"] = json!(next); t["status"] = json!("todo"); t["claim"] = Value::Null; touch(t, now);
                    detail = Some(format!("repeats {r}: next due {}", chrono::Utc.timestamp_millis_opt(next).single().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()));
                } else { if t["status"] != json!("done") { t["doneAt"] = json!(now); } t["status"] = json!("done"); t["claim"] = Value::Null; touch(t, now); detail = None; }
                let tc = t.clone();
                log(db, Some(&tc), a, "completed", detail); self.save(db);
                Out { status: 200, body: json!({ "task": card(&tc, now) }), changes: change("completed", &tc) }
            }
            "claim" => {
                let Some(t) = find(db, &id) else { return no(404, "No such task.") };
                if held_by_other(t, a, now) { return no(409, format!("{} already holds {} until {}.", t["claim"]["by"].as_str().unwrap_or(""), id, time_of(t["claim"]["until"].as_i64().unwrap_or(0)))); }
                let minutes = b["minutes"].as_f64().filter(|m| *m > 0.0).unwrap_or(CLAIM_DEFAULT_MIN).clamp(1.0, CLAIM_MAX_MIN);
                let renew = claim_active(t, now) && t["claim"]["key"] == json!(a.key());
                let t = find_mut(db, &id).unwrap();
                let until = now + (minutes * 60_000.0) as i64;
                t["claim"] = json!({ "key": a.key(), "by": a.label(), "agent": a.agent, "user": a.user, "until": until, "at": if renew { t["claim"]["at"].as_i64().unwrap_or(now) } else { now } });
                if !renew && (t["status"] == json!("todo") || t["status"] == json!("backlog")) { t["status"] = json!("doing"); }
                touch(t, now);
                let tc = t.clone();
                if !renew { log(db, Some(&tc), a, "claimed", Some(format!("{} min", minutes))); }
                self.save(db);
                ok(json!({ "task": card(&tc, now), "until": until }))
            }
            "release" => {
                let Some(t) = find(db, &id) else { return no(404, "No such task.") };
                if held_by_other(t, a, now) { return no(409, format!("{} holds {}; only they can release it (or wait for it to lapse).", t["claim"]["by"].as_str().unwrap_or(""), id)); }
                let t = find_mut(db, &id).unwrap();
                if !t["claim"].is_null() {
                    t["claim"] = Value::Null; touch(t, now);
                    let tc = t.clone();
                    log(db, Some(&tc), a, "released", b["note"].as_str().map(String::from)); self.save(db);
                    return ok(json!({ "task": card(&tc, now) }));
                }
                ok(json!({ "task": card(t, now) }))
            }
            "remove" => {
                if let Some(g) = guard(find(db, &id), a, None, now) { return g; }
                let t = find_mut(db, &id).unwrap();
                t["deleted"] = json!(true); t["claim"] = Value::Null; touch(t, now);
                let tc = t.clone();
                log(db, Some(&tc), a, "deleted", None); self.save(db);
                Out { status: 200, body: json!({ "ok": true }), changes: change("deleted", &tc) }
            }
            // ---- the archive (T-190): a done task leaves the board and the calendar, stays readable and restorable ----
            "archive" => {
                if let Some(g) = guard(find(db, &id), a, None, now) { return g; }
                let t = find_mut(db, &id).unwrap();
                if t["status"] != json!("done") { return no(400, format!("{id} is not done: only a done task can be archived (complete it first).")); }
                if is_archived(t) { return ok(json!({ "task": card(t, now) })); }
                t["archivedAt"] = json!(now); t["claim"] = Value::Null; touch(t, now);
                let tc = t.clone();
                log(db, Some(&tc), a, "archived", None); self.save(db);
                ok(json!({ "task": card(&tc, now) }))
            }
            "restore" => {
                if let Some(g) = guard(find(db, &id), a, None, now) { return g; }
                let to = b["status"].as_str().filter(|s| STATUS_IDS.contains(s)).map(String::from);
                let t = find_mut(db, &id).unwrap();
                if !is_archived(t) { return no(400, format!("{id} is not in the archive.")); }
                unarchive(t);
                match to.as_deref() { Some(s) if s != "done" => { t["status"] = json!(s); } _ => { t["doneAt"] = json!(now); } } // back on the board for another full stay
                touch(t, now);
                let tc = t.clone();
                log(db, Some(&tc), a, "restored", Some(format!("to {}", tc["status"].as_str().unwrap_or("done")))); self.save(db);
                Out { status: 200, body: json!({ "task": card(&tc, now) }), changes: if tc["status"] != json!("done") { change("updated", &tc) } else { vec![] } }
            }
            "archiveDone" => {
                let bd = b["board"].as_str().filter(|s| !s.is_empty()).map(String::from);
                let mut done: Vec<Value> = vec![];
                for t in db["tasks"].as_array_mut().into_iter().flatten().filter(|t| shown(t) && t["status"] == json!("done") && !held_by_other(t, a, now)) {
                    if let Some(bd) = &bd { if t["board"] != json!(bd) { continue; } }
                    t["archivedAt"] = json!(now); t["claim"] = Value::Null; touch(t, now); done.push(t.clone());
                }
                for t in &done { log(db, Some(t), a, "archived", Some("archive all done".into())); }
                if !done.is_empty() { self.save(db); }
                ok(json!({ "archived": done.iter().map(|t| t["id"].clone()).collect::<Vec<_>>() }))
            }
            "archived" => {
                let (_, del_after) = self.retention();
                let q = b["q"].as_str().unwrap_or("").trim().to_lowercase();
                let mut tasks: Vec<Value> = db["tasks"].as_array().unwrap().iter().filter(|t| live(t) && is_archived(t)).filter(|t| {
                    if let Some(bd) = b["board"].as_str().filter(|s| !s.is_empty()) { if t["board"] != json!(bd) { return false; } }
                    q.is_empty() || q.split_whitespace().all(|w| t["id"].as_str().unwrap_or("").to_lowercase() == w || t["title"].as_str().unwrap_or("").to_lowercase().contains(w)
                        || t["labels"].as_array().map(|l| l.iter().any(|x| x.as_str().unwrap_or("").to_lowercase() == w)).unwrap_or(false))
                }).map(|t| { let mut c = card(t, now); let at = t["archivedAt"].as_i64().unwrap_or(now); c["deleteAt"] = if del_after > 0 { json!(at + del_after) } else { Value::Null }; c }).collect();
                tasks.sort_by_key(|t| std::cmp::Reverse(t["archivedAt"].as_i64().unwrap_or(0)));
                let n = tasks.len();
                ok(json!({ "boards": db["boards"], "tasks": tasks, "archive": self.archive_info(n) }))
            }
            "newBoard" if b["_noNewBoards"] == json!(true) => no(400, "Boards here are the servers' own: every server has one."),
            "newBoard" => {
                let name: String = b["name"].as_str().map(|s| s.trim().chars().take(40).collect()).unwrap_or_default();
                if name.is_empty() { return no(400, "A board needs a name."); }
                let mut bid = String::new(); let mut dash = false;
                for c in name.to_lowercase().chars() { if c.is_ascii_lowercase() || c.is_ascii_digit() { bid.push(c); dash = false; } else if !dash { bid.push('-'); dash = true; } }
                let bid = { let s = bid.trim_matches('-').to_string(); if s.is_empty() { "board".to_string() } else { s } };
                if db["boards"].as_array().unwrap().iter().any(|x| x["id"] == json!(bid)) { return no(400, "That board exists already."); }
                db["boards"].as_array_mut().unwrap().push(json!({ "id": bid, "name": name }));
                log(db, None, a, "created board", Some(name)); self.save(db);
                ok(json!({ "boards": db["boards"] }))
            }
            "list" => {
                let mut tasks: Vec<Value> = db["tasks"].as_array().unwrap().iter().filter(|t| shown(t)).map(|t| card(t, now)).collect();
                if let Some(bd) = b["board"].as_str().filter(|s| !s.is_empty()) { tasks.retain(|t| t["board"] == json!(bd)); }
                if let Some(st) = b["status"].as_str().filter(|s| !s.is_empty()) { tasks.retain(|t| t["status"] == json!(st)); }
                if b["mine"].as_bool() == Some(true) { tasks.retain(|t| t["assignee"] == json!(a.user) || t["claim"]["key"] == json!(a.key())); }
                ok(json!({ "boards": db["boards"], "statuses": STATUS_IDS, "tasks": tasks }))
            }
            "calendar" => {
                let from = if b.get("from").is_some() { parse_due(&b["from"]) } else { parse_due(&json!("today")) };
                let to = if b.get("to").is_some() { parse_due(&b["to"]) } else { parse_due(&json!("+14d")) };
                let (from, to) = match (from, to) { (Ok(Some(f)), Ok(Some(t))) => (f, t), (Err(e), _) | (_, Err(e)) => return no(400, e), _ => return no(400, "from / to must be dates.") };
                let sd = local(from).date_naive(); let ed = local(to).date_naive();
                let start = at(sd.year(), sd.month(), sd.day(), 0, 0).unwrap_or(from);
                let end = at(ed.year(), ed.month(), ed.day(), 23, 59).unwrap_or(to) + 59_999;
                let mut by_day: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
                for t in db["tasks"].as_array().unwrap().iter().filter(|t| shown(t)) {
                    let Some(due) = t["dueAt"].as_i64() else { continue };
                    if due < start || due > end { continue; }
                    if let Some(bd) = b["board"].as_str().filter(|s| !s.is_empty()) { if t["board"] != json!(bd) { continue; } }
                    let mut c = card(t, now);
                    c["overdue"] = json!(t["status"] != json!("done") && due < now);
                    c["assigneeName"] = t["assignee"].as_str().map(|u| json!(name_of(u))).unwrap_or(Value::Null);
                    by_day.entry(day_key(due)).or_default().push(c);
                }
                let days: Vec<Value> = by_day.into_iter().map(|(day, mut tasks)| {
                    tasks.sort_by_key(|t| t["dueAt"].as_i64().unwrap_or(0));
                    let wd = NaiveDate::parse_from_str(&day, "%Y-%m-%d").map(|d| DAYS[d.weekday().num_days_from_sunday() as usize]).unwrap_or("");
                    json!({ "day": day, "weekday": wd, "tasks": tasks })
                }).collect();
                let overdue: Vec<Value> = db["tasks"].as_array().unwrap().iter().filter(|t| shown(t) && t["status"] != json!("done") && t["dueAt"].as_i64().map(|d| d < start).unwrap_or(false)).map(|t| card(t, now)).collect();
                ok(json!({ "from": day_key(start), "to": day_key(end), "today": day_key(now), "days": days, "overdue": overdue }))
            }
            _ => no(404, "Unknown board operation."),
        }
    }

    // ---------- for the Forgejo mirror ----------
    pub fn set_external(&self, id: &str, ext: Option<Value>) {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = db["tasks"].as_array_mut().and_then(|ts| ts.iter_mut().find(|t| t["id"] == json!(id))) {
            match ext { Some(e) => { t["forgejo"] = e; } None => { if let Some(o) = t.as_object_mut() { o.remove("forgejo"); } } }
            self.save(&mut db);
        }
    }
    pub fn external(&self, id: &str) -> Option<Value> { let db = self.db.lock().unwrap_or_else(|e| e.into_inner()); db["tasks"].as_array()?.iter().find(|t| t["id"] == json!(id)).and_then(|t| t.get("forgejo").cloned()).filter(|v| !v.is_null()) }
    pub fn by_external(&self, number: i64) -> Option<Value> { let db = self.db.lock().unwrap_or_else(|e| e.into_inner()); db["tasks"].as_array()?.iter().find(|t| live(t) && t["forgejo"]["number"].as_i64() == Some(number)).cloned() }
    /// What each person holds right now (live claims, their own or their Claude's) and how many open tasks are
    /// assigned to them: user → (held task ids, assigned open tasks). For the presence on the board (T-262).
    pub fn holdings(&self) -> HashMap<String, (Vec<String>, usize)> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner()); let now = now_ms() as i64;
        let mut m: HashMap<String, (Vec<String>, usize)> = HashMap::new();
        for t in db["tasks"].as_array().into_iter().flatten().filter(|t| shown(t) && t["status"] != json!("done")) {
            if claim_active(t, now) { if let Some(u) = t["claim"]["user"].as_str() { m.entry(u.to_string()).or_default().0.push(t["id"].as_str().unwrap_or("").to_string()); } }
            if let Some(u) = t["assignee"].as_str() { m.entry(u.to_string()).or_default().1 += 1; }
        }
        m
    }
    pub fn get(&self, id: &str) -> Option<Value> { let db = self.db.lock().unwrap_or_else(|e| e.into_inner()); db["tasks"].as_array()?.iter().find(|t| t["id"] == json!(id)).cloned() }

    // ---------- for a sync with another copy of the board (T-324: a dev hub next to the live one) ----------
    // A remote change lands in memory and on disk at once and wakes the long poll (the same save as every operation).
    /// Every task as stored, deleted ones too (their tombstones travel).
    pub fn sync_tasks(&self) -> Vec<Value> { self.db.lock().unwrap_or_else(|e| e.into_inner())["tasks"].as_array().cloned().unwrap_or_default() }
    /// Another copy's version of a task, stored as it is (this copy's is replaced); the next task number stays above it.
    pub fn sync_put_task(&self, t: &Value) -> Value {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let id = t["id"].as_str().unwrap_or("").to_string();
        match db["tasks"].as_array_mut().unwrap().iter_mut().find(|x| x["id"] == json!(id)) { Some(x) => *x = t.clone(), None => db["tasks"].as_array_mut().unwrap().push(t.clone()) }
        let n = task_number(&id).max(db["n"].as_i64().unwrap_or(0)); db["n"] = json!(n);
        self.save(&mut db);
        t.clone()
    }
    /// This copy's task `id` moves to a new number (the other copy made a different task with the same number at the same
    /// time, and its number stays); the new id. Logged in the activity.
    pub fn sync_rekey(&self, id: &str) -> Option<String> {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let top = db["tasks"].as_array()?.iter().map(|t| task_number(t["id"].as_str().unwrap_or(""))).max().unwrap_or(0).max(db["n"].as_i64().unwrap_or(0));
        let new_id = format!("T-{}", top + 1);
        let t = db["tasks"].as_array_mut()?.iter_mut().find(|t| t["id"] == json!(id))?;
        t["id"] = json!(new_id);
        let tc = t.clone();
        db["n"] = json!(top + 1);
        log(&mut db, Some(&tc), &Actor { user: "sync".into(), agent: false, name: "Sync".into() }, "renumbered", Some(format!("was {id}: the other hub made a different {id} at the same time")));
        self.save(&mut db);
        Some(new_id)
    }
    /// The activity log as stored.
    pub fn sync_activity(&self) -> Vec<Value> { self.db.lock().unwrap_or_else(|e| e.into_inner())["activity"].as_array().cloned().unwrap_or_default() }
    /// An activity entry from the other copy, in time order (an identical one already here is not added twice).
    pub fn sync_add_activity(&self, e: &Value) {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let a = db["activity"].as_array_mut().unwrap();
        if a.iter().any(|x| x == e) { return; }
        let at = e["at"].as_i64().unwrap_or(0);
        let pos = a.iter().rposition(|x| x["at"].as_i64().unwrap_or(0) <= at).map(|p| p + 1).unwrap_or(0);
        a.insert(pos, e.clone());
        self.save(&mut db);
    }
    /// The boards (id + name).
    pub fn sync_boards(&self) -> Vec<Value> { self.db.lock().unwrap_or_else(|e| e.into_inner())["boards"].as_array().cloned().unwrap_or_default() }
    /// A board from the other copy (added, or renamed to its name).
    pub fn sync_put_board(&self, b: &Value) {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let boards = db["boards"].as_array_mut().unwrap();
        match boards.iter_mut().find(|x| x["id"] == b["id"]) { Some(x) => { if x == b { return; } *x = b.clone(); } None => boards.push(b.clone()) }
        self.save(&mut db);
    }
}

/// A task's number (T-12 → 12; 0 when it has none).
fn task_number(id: &str) -> i64 { id.strip_prefix("T-").and_then(|n| n.parse().ok()).unwrap_or(0) }
/// The shape a task from another copy must have before it is stored (T-324): what the board and both hubs read.
pub fn sync_task_ok(id: &str, t: &Value) -> Result<(), String> {
    let o = t.as_object().ok_or("a task is an object")?;
    if t["id"].as_str() != Some(id) || task_number(id) < 1 { return Err("its id is not T-<number> or not the record's".into()); }
    let title = t["title"].as_str().ok_or("no title")?;
    if title.trim().is_empty() || title.chars().count() > 200 { return Err("the title is empty or longer than 200 characters".into()); }
    if !t["board"].as_str().is_some_and(|b| !b.is_empty() && b.len() <= 60) { return Err("no board".into()); }
    if !t["status"].as_str().is_some_and(|s| STATUS_IDS.contains(&s)) { return Err("an unknown status".into()); }
    if !t["rev"].as_i64().is_some_and(|r| r >= 1) { return Err("no revision".into()); }
    for k in ["createdAt", "updatedAt"] { if !t[k].is_i64() && !t[k].is_u64() { return Err(format!("{k} is not a time")); } }
    if o.get("body").is_some_and(|b| !b.is_null() && !b.as_str().is_some_and(|s| s.len() <= 4_000_000)) { return Err("the description is not text (or too long)".into()); }
    if o.get("labels").is_some_and(|l| !l.is_null() && !l.is_array()) { return Err("labels are not a list".into()); }
    if o.get("priority").is_some_and(|p| !p.is_null() && !p.as_i64().is_some_and(|p| (0..=5).contains(&p))) { return Err("priority is not 0 to 5".into()); }
    if o.get("deleted").is_some_and(|d| !d.is_boolean()) { return Err("deleted is not true / false".into()); }
    Ok(())
}
/// The activity entry that keeps the losing side of a sync conflict on a task (T-324): who won, and the loser's content
/// in full (`lost`). The same on both copies (made from the two versions only), so it is kept once. None when nothing
/// of value lost (the two differed only in claim, archive time, revision or time: say both hubs' sweeps archived it).
pub fn sync_conflict_entry(id: &str, winner: Option<&Value>, loser: Option<&Value>, winner_from: &str, loser_from: &str) -> Option<Value> {
    let l = loser?;
    const CONTENT: [&str; 10] = ["title", "body", "status", "labels", "priority", "dueAt", "recur", "assignee", "board", "deleted"];
    let w = winner.cloned().unwrap_or(Value::Null);
    if CONTENT.iter().all(|k| w[*k] == l[*k]) { return None; }
    let mut lost = serde_json::Map::new();
    for k in CONTENT.iter().chain(["rev", "updatedAt"].iter()) { if let Some(v) = l.get(*k) { lost.insert((*k).to_string(), v.clone()); } }
    lost.insert("from".into(), json!(loser_from));
    let at = w["updatedAt"].as_i64().unwrap_or(0).max(l["updatedAt"].as_i64().unwrap_or(0));
    Some(json!({ "at": at, "task": id, "title": if w["title"].is_string() { w["title"].clone() } else { l["title"].clone() }, "by": "Sync", "agent": false, "what": "sync conflict",
        "detail": format!("{winner_from} and {loser_from} changed it at the same time: {winner_from}'s change stays; {loser_from}'s is kept here in full"), "lost": Value::Object(lost) }))
}

fn live(t: &Value) -> bool { t["deleted"].as_bool() != Some(true) }
fn is_archived(t: &Value) -> bool { t["archivedAt"].as_i64().is_some() }
/// On the board (and the calendar): live and not archived.
fn shown(t: &Value) -> bool { live(t) && !is_archived(t) }
/// When a done task was finished: doneAt (since 5.32.0), else its last change.
fn done_since(t: &Value) -> i64 { t["doneAt"].as_i64().or_else(|| t["updatedAt"].as_i64()).unwrap_or(0) }
/// Takes a task out of the archive (and forgets when it was done); whether it was in it.
fn unarchive(t: &mut Value) -> bool {
    let Some(o) = t.as_object_mut() else { return false };
    o.remove("doneAt");
    o.remove("archivedAt").map(|v| !v.is_null()).unwrap_or(false)
}
fn find<'a>(db: &'a Value, id: &str) -> Option<&'a Value> { db["tasks"].as_array()?.iter().find(|t| t["id"] == json!(id) && live(t)) }
fn find_mut<'a>(db: &'a mut Value, id: &str) -> Option<&'a mut Value> { db["tasks"].as_array_mut()?.iter_mut().find(|t| t["id"] == json!(id) && live(t)) }
fn claim_active(t: &Value, now: i64) -> bool { t["claim"].is_object() && t["claim"]["until"].as_i64().unwrap_or(0) > now }
fn held_by_other(t: &Value, a: &Actor, now: i64) -> bool { claim_active(t, now) && t["claim"]["key"] != json!(a.key()) }
fn touch(t: &mut Value, now: i64) { t["rev"] = json!(t["rev"].as_i64().unwrap_or(0) + 1); t["updatedAt"] = json!(now); }
fn clamp_pri(v: &Value) -> i64 { (v.as_f64().unwrap_or(0.0) as i64).clamp(0, 5) }
fn clean_labels(v: &Value) -> Value {
    let items: Vec<String> = match v { Value::Array(a) => a.iter().map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).collect(), Value::Null => vec![], other => other.as_str().unwrap_or("").split(',').map(String::from).collect() };
    json!(items.into_iter().map(|s| s.trim().chars().take(32).collect::<String>()).filter(|s| !s.is_empty()).take(12).collect::<Vec<_>>())
}
fn guard(t: Option<&Value>, a: &Actor, base_rev: Option<&Value>, now: i64) -> Option<Out> {
    let Some(t) = t else { return Some(no(404, "No such task.")) };
    if held_by_other(t, a, now) { return Some(no(409, format!("{} holds a claim on {} until {}. Edits are refused until it is released or lapses.", t["claim"]["by"].as_str().unwrap_or(""), t["id"].as_str().unwrap_or(""), time_of(t["claim"]["until"].as_i64().unwrap_or(0))))); }
    if let Some(r) = base_rev { let want = r.as_f64().or_else(|| r.as_str().and_then(|s| s.parse().ok())).unwrap_or(f64::NAN); if want as i64 != t["rev"].as_i64().unwrap_or(0) || want.is_nan() { return Some(no(409, format!("{} changed since you loaded it (revision {}, you had {}). Reload it and try again.", t["id"].as_str().unwrap_or(""), t["rev"], r))); } }
    None
}
fn log(db: &mut Value, t: Option<&Value>, a: &Actor, what: &str, detail: Option<String>) {
    let mut e = json!({ "at": now_ms(), "task": t.map(|t| t["id"].clone()).unwrap_or(Value::Null), "title": t.map(|t| t["title"].clone()).unwrap_or(json!("")), "by": a.label(), "agent": a.agent, "what": what });
    if let Some(d) = detail { e["detail"] = json!(d.chars().take(300).collect::<String>()); }
    db["activity"].as_array_mut().unwrap().push(e);
}
/// A list entry: no prose (the description is fetched per task).
fn card(t: &Value, now: i64) -> Value {
    let claim = if claim_active(t, now) { json!({ "by": t["claim"]["by"], "agent": t["claim"]["agent"], "until": t["claim"]["until"], "key": t["claim"]["key"] }) } else { Value::Null };
    let fj = if t["forgejo"].is_object() { json!({ "number": t["forgejo"]["number"], "url": t["forgejo"]["url"], "state": t["forgejo"]["state"] }) } else { Value::Null };
    let or_null = |v: &Value| if v.is_null() { Value::Null } else { v.clone() };
    json!({ "id": t["id"], "board": t["board"], "title": t["title"], "status": t["status"], "order": t["order"], "priority": t["priority"].as_i64().unwrap_or(0), "labels": if t["labels"].is_array() { t["labels"].clone() } else { json!([]) },
        "dueAt": or_null(&t["dueAt"]), "recur": or_null(&t["recur"]), "assignee": or_null(&t["assignee"]), "createdBy": t["createdBy"], "updatedAt": t["updatedAt"], "rev": t["rev"],
        "hasBody": !t["body"].as_str().unwrap_or("").is_empty(), "claim": claim, "stale": t["claim"].is_object() && !claim_active(t, now), "forgejo": fj, "archivedAt": or_null(&t["archivedAt"]) })
}
/// The MCP tool names → operations (bridge/board.js internal).
pub fn tool_op(name: &str) -> Option<&'static str> {
    Some(match name { "board_list" => "list", "task_get" => "get", "task_create" => "create", "task_update" => "update", "task_claim" | "task_renew" => "claim", "task_release" => "release", "task_complete" => "complete", "task_delete" => "remove", "task_move" => "move", "board_new" => "newBoard", "board_calendar" => "calendar",
        "task_archive" => "archive", "task_restore" => "restore", "board_archive_done" => "archiveDone", "board_archived" => "archived", _ => return None })
}
/// The HTTP routes for people → operations.
pub fn route_op(path: &str) -> Option<&'static str> {
    Some(match path { "/api/board/create" => "create", "/api/board/task" => "get", "/api/board/update" => "update", "/api/board/move" => "move", "/api/board/complete" => "complete", "/api/board/claim" => "claim", "/api/board/release" => "release", "/api/board/delete" => "remove", "/api/board/new" => "newBoard",
        "/api/board/archive" => "archive", "/api/board/restore" => "restore", "/api/board/archive-done" => "archiveDone", "/api/board/archived" => "archived", _ => return None })
}
#[allow(dead_code)]
pub fn obj(v: &Value) -> Map<String, Value> { v.as_object().cloned().unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn month_end_repeats_like_the_javascript() {
        let jan31 = at(2026, 1, 31, 17, 0).unwrap();
        let feb = next_due(jan31, "monthly", 31).unwrap(); assert_eq!(day_key(feb), "2026-02-28");
        let mar = next_due(feb, "monthly", 31).unwrap(); assert_eq!(day_key(mar), "2026-03-31");
        let leap = next_due(at(2028, 1, 31, 17, 0).unwrap(), "monthly", 31).unwrap(); assert_eq!(day_key(leap), "2028-02-29");
        assert_eq!(day_key(next_due(jan31, "yearly", 31).unwrap()), "2027-01-31");
    }
    fn scratch(name: &str) -> (Board, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pgm-board-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Board::new(dir.clone()), dir)
    }
    fn op(b: &Board, op: &str, body: Value) -> Out { b.op(op, &Actor { user: "ann".into(), agent: false, name: "Ann".into() }, &body, &|u| u == "ann", &|u| u.to_string()) }
    fn ids(v: &Value) -> Vec<String> { v["tasks"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect() }
    fn made(b: &Board, body: Value) -> String { op(b, "create", body).body["task"]["id"].as_str().unwrap().to_string() }

    #[test]
    fn the_sweep_archives_done_tasks_after_48_hours_and_deletes_them_30_days_later() {
        let (b, dir) = scratch("sweep");
        let now = now_ms() as i64; const H: i64 = 3_600_000;
        let plain = made(&b, json!({ "title": "plain" }));
        let repeat = made(&b, json!({ "title": "repeats, no due date", "recur": "weekly" }));
        let future = made(&b, json!({ "title": "due next month", "dueAt": now + 30 * 24 * H }));
        let held = made(&b, json!({ "title": "held" }));
        let open = made(&b, json!({ "title": "still open" }));
        for id in [&plain, &repeat, &future, &held] { assert_eq!(op(&b, "complete", json!({ "id": id })).status, 200); }
        op(&b, "claim", json!({ "id": held, "minutes": 240 }));
        assert_eq!(b.sweep(now + 47 * H), (vec![], vec![]), "nothing before 48 h");
        b.set_retention(H, DELETE_AFTER_MS); // a claim lasts 4 h at most: look while it is still live
        let (arch, del) = b.sweep(now + 2 * H);
        assert_eq!(arch, vec![plain.clone()], "only the plain done task: never a repeating one, one due in the future, one held or one open");
        assert!(del.is_empty());
        b.set_retention(ARCHIVE_AFTER_MS, DELETE_AFTER_MS);
        assert_eq!(b.sweep(now + 49 * H).0, vec![held.clone()], "a held one once its claim lapsed");
        let at = now + 2 * H; // when the plain one was archived
        assert!(!ids(&b.view("ann", &|u| u.to_string(), &|_| true, Value::Null)).contains(&plain), "an archived task leaves the board");
        assert!(!ids(&op(&b, "list", json!({})).body).contains(&plain), "and the list");
        assert_eq!(op(&b, "get", json!({ "id": plain })).body["task"]["archivedAt"], json!(at), "it can still be read");
        let ar = op(&b, "archived", json!({})).body;
        assert_eq!(ids(&ar), vec![held.clone(), plain.clone()], "newest first");
        assert_eq!(ar["tasks"][1]["deleteAt"], json!(at + DELETE_AFTER_MS), "the archive says when it will be deleted");
        let left = ids(&b.view("ann", &|u| u.to_string(), &|_| true, Value::Null));
        assert!(left.contains(&open) && left.contains(&repeat) && left.contains(&future));
        assert_eq!(b.sweep(at + DELETE_AFTER_MS - 1).1, Vec::<String>::new(), "kept for the full 30 days");
        let (_, del) = b.sweep(at + DELETE_AFTER_MS);
        assert_eq!(del, vec![plain.clone()]);
        assert_eq!(op(&b, "get", json!({ "id": plain })).status, 404, "deleted");
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("board.json")).unwrap()).unwrap();
        let t = disk["tasks"].as_array().unwrap().iter().find(|t| t["id"] == json!(plain)).unwrap();
        assert_eq!(t["deleted"], json!(true), "the usual tombstone stays in the file");
        assert!(disk["activity"].as_array().unwrap().iter().any(|e| e["task"] == json!(plain) && e["what"] == json!("deleted") && e["by"] == json!("Board")));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn archive_by_hand_restore_and_archive_all_done() {
        let (b, dir) = scratch("hand");
        let a = made(&b, json!({ "title": "alpha", "labels": ["ui"], "due": "today" }));
        let c = made(&b, json!({ "title": "gamma" }));
        let open = made(&b, json!({ "title": "open" }));
        assert_eq!(op(&b, "archive", json!({ "id": open })).status, 400, "an open task is not archived");
        op(&b, "complete", json!({ "id": a })); op(&b, "complete", json!({ "id": c }));
        let on_cal = |b: &Board| op(b, "calendar", json!({ "from": "today", "to": "+1d" })).body.to_string().contains(&format!("\"{a}\""));
        assert!(on_cal(&b), "a done task with a due date is on the calendar");
        assert_eq!(op(&b, "archive", json!({ "id": a })).status, 200);
        assert!(!on_cal(&b), "an archived one is not");
        assert_eq!(ids(&op(&b, "archived", json!({ "q": "ui" })).body), vec![a.clone()], "search by label");
        assert!(ids(&op(&b, "archived", json!({ "q": "nothing-like-it" })).body).is_empty());
        assert_eq!(op(&b, "restore", json!({ "id": c })).status, 400, "a task not in the archive cannot be restored");
        let r = op(&b, "restore", json!({ "id": a, "status": "review" }));
        assert_eq!((r.status, r.body["task"]["status"].clone(), r.body["task"]["archivedAt"].clone()), (200, json!("review"), Value::Null));
        op(&b, "complete", json!({ "id": a }));
        let all = op(&b, "archiveDone", json!({})).body;
        assert_eq!(all["archived"].as_array().unwrap().len(), 2);
        assert_eq!(ids(&op(&b, "list", json!({})).body), vec![open.clone()]);
        let back = op(&b, "update", json!({ "id": c, "status": "todo" }));
        assert_eq!((back.body["task"]["status"].clone(), back.body["task"]["archivedAt"].clone()), (json!("todo"), Value::Null), "moving an archived task off done restores it");
        b.set_retention(0, 0);
        assert_eq!(b.sweep(now_ms() as i64 + 400 * 86_400_000), (vec![], vec![]), "0 = never");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn another_copys_task_lands_in_memory_and_on_disk_and_a_clash_of_numbers_renumbers_this_copys() {
        let (b, dir) = scratch("sync");
        let mine = made(&b, json!({ "title": "mine" }));
        let v0 = b.version();
        let theirs = json!({ "id": "T-5", "board": "team", "title": "theirs", "status": "todo", "rev": 3, "createdAt": 1, "updatedAt": 2, "createdBy": "Bob", "claim": null });
        assert!(sync_task_ok("T-5", &theirs).is_ok());
        b.sync_put_task(&theirs);
        assert!(b.version() > v0, "the long poll wakes");
        assert_eq!(op(&b, "get", json!({ "id": "T-5" })).body["task"]["title"], "theirs");
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("board.json")).unwrap()).unwrap();
        assert!(disk["tasks"].as_array().unwrap().iter().any(|t| t["id"] == "T-5"), "on disk too");
        assert_eq!(made(&b, json!({ "title": "next" })), "T-6", "numbers continue above it");
        let moved = b.sync_rekey(&mine).unwrap();
        assert_eq!(moved, "T-7");
        assert_eq!(op(&b, "get", json!({ "id": moved })).body["task"]["title"], "mine");
        assert!(b.sync_activity().iter().any(|e| e["what"] == "renumbered" && e["task"] == json!(moved)));
        // shape checks
        for (bad, why) in [(json!({ "id": "T-5", "board": "team", "title": "", "status": "todo", "rev": 1, "createdAt": 1, "updatedAt": 1 }), "empty title"), (json!({ "id": "T-5", "board": "team", "title": "x", "status": "nope", "rev": 1, "createdAt": 1, "updatedAt": 1 }), "status"), (json!({ "id": "T-6", "board": "team", "title": "x", "status": "todo", "rev": 1, "createdAt": 1, "updatedAt": 1 }), "another id"), (json!({ "id": "T-5", "board": "team", "title": "x", "status": "todo", "createdAt": 1, "updatedAt": 1 }), "no rev")] {
            assert!(sync_task_ok("T-5", &bad).is_err(), "{why}");
        }
        // a conflict's loser goes into the activity in full; a difference in archive time only is no loss
        let w = json!({ "title": "A", "body": "x", "status": "todo", "rev": 4, "updatedAt": 10 });
        let l = json!({ "title": "B", "body": "y", "status": "todo", "rev": 4, "updatedAt": 9 });
        let e = sync_conflict_entry("T-5", Some(&w), Some(&l), "dev", "live").unwrap();
        assert_eq!((e["lost"]["title"].clone(), e["lost"]["body"].clone(), e["lost"]["from"].clone(), e["at"].clone()), (json!("B"), json!("y"), json!("live"), json!(10)));
        assert_eq!(e, sync_conflict_entry("T-5", Some(&w), Some(&l), "dev", "live").unwrap(), "the same entry on both copies");
        let mut l2 = w.clone(); l2["archivedAt"] = json!(5); l2["updatedAt"] = json!(11);
        assert!(sync_conflict_entry("T-5", Some(&w), Some(&l2), "dev", "live").is_none());
        b.sync_add_activity(&e); b.sync_add_activity(&e);
        assert_eq!(b.sync_activity().iter().filter(|x| **x == e).count(), 1, "kept once");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn due_dates() {
        assert_eq!(parse_due(&json!("none")).unwrap(), None);
        assert_eq!(day_key(parse_due(&json!("2026-03-05")).unwrap().unwrap()), "2026-03-05");
        assert_eq!(local(parse_due(&json!("2026-03-05 09:30")).unwrap().unwrap()).hour(), 9);
        assert!(parse_due(&json!("someday maybe")).is_err());
        assert!(parse_due(&json!("+3d")).unwrap().is_some() && parse_due(&json!("next friday")).unwrap().is_some() && parse_due(&json!("in 2 weeks")).unwrap().is_some());
        assert_eq!(local(parse_due(&json!("tomorrow")).unwrap().unwrap()).hour(), 17);
    }
}
