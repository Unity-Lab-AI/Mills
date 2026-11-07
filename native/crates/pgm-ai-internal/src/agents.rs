//! Agent to agent (T-296 / T-297; team note plans/agent-talk.md): every person has ONE agent address, `@<username>`, which
//! is whichever session of theirs is current (section 2). An agent message is a typed envelope around plain text (section 3)
//! that the hub puts into that session as a message, the way the person's own message arrives: queued behind a working run,
//! or waking an idle session (a run starts with it, as that person, section 4). The receiving run answers with the same
//! tools. The hub keeps a thread per pair of people on both sides, an audit trail, and the person's controls (mute, pause,
//! ask me first, wake me; section 5), with hop / rate limits and a loop detector (section 8).
//!
//! Storage (add-only; the frozen JavaScript hub never reads it, it answers 404 on /api/agents/*):
//! - `<profile>/agents/settings.json` = `{muted, paused, askFirst, wake}`
//! - `<profile>/agents/threads/<thread>.json` = `{id, with, session, subject, status, why, createdAt, lastAt, hopReset, seenAt,
//!   messages}`, written on both people's sides in one go (a refused attempt of a muted person only on the muter's side)
//! - `Chat/agent-audit.ndjson`, append-only, one line per event, rotated monthly (kept 12 months)
//!
//! Queued messages wait in the thread store (status `queued`), not in the page's queue list: the page's `POST
//! /api/work/queue` replaces that list as a whole, so the hub delivers its own when the session is free (the delivery pump).
//! A message for an external session (Jcode PGM, T-299) stays queued here for the inbox route: `GET /api/agents/inbox?source=
//! &device=&wait=1` (fetched = delivered) and `POST /api/agents/read {ids}`; that tool sends and replies with `source` (and its
//! reported `session`) on /api/agents/send and /reply, and the answers to its asks wait in its inbox. A run woken by an ask whose
//! sender waits and that never calls agent_reply (a model without tools) has its final answer sent as the reply (`x.by:
//! "final-answer"`, section 7). An agent-started run is the receiver's: their time log and usage ledger note it (section 8).
use bytes::Bytes;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use crate::hub::{answer, AiHub, Resp};
use crate::sessions::{self, Sx};
use crate::work::Caller;
use pgm_core_internal::json::{read_json, s, write_json};
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

/// The kinds of a message (section 3).
pub const KINDS: [&str; 5] = ["ask", "handoff", "review", "report", "reply"];
/// The kinds "ask me first" may hold (a reply or a report is never held).
pub const HOLDABLE: [&str; 3] = ["handoff", "ask", "review"];
/// A message's text, at most (cut with "…" and said so).
pub const TEXT_MAX: usize = 20_000;
/// A thread holds at most this many messages, then it is stopped ("full").
pub const THREAD_MAX: usize = 500;
/// How long one HTTP call (a tool call, a page's send with wait) is held before it answers `{pending}` (as ask_user).
pub const POLL_SEC: u64 = 240;
/// The longest a sender's wait blocks in all (then the reply comes later, as a message).
pub const WAIT_MAX_MS: u64 = 10 * 60_000;
/// A send this soon after a refused tool in the sender's run carries afterRefusal (section 5).
pub const AFTER_REFUSAL_MS: u64 = 120_000;
/// How long the inbox long poll of another tool (GET /api/agents/inbox?wait=1, T-299) holds before it answers an empty list.
pub const INBOX_WAIT_SEC: u64 = 25;
/// The fields of the envelope, all of them, in both directions (nothing model-specific). The hub's own notes on a message
/// (afterRefusal, a decline's note, whether the text was cut) travel beside it in `x`, which [`envelope`] leaves out.
pub const ENVELOPE: [&str; 13] = ["id", "thread", "from", "to", "kind", "text", "refs", "hop", "parent", "replyBy", "at", "status", "why"];
const SHARED: [&str; 6] = ["subject", "status", "why", "createdAt", "lastAt", "hopReset"];

// ---------- limits (config agents {…}, section 8) ----------
#[derive(Clone, Debug, PartialEq)]
pub struct Limits { pub max_hops: u64, pub per_pair: usize, pub per_sender: usize, pub max_agent_runs: usize, pub loop_window: usize, pub loop_similarity: f64, pub reply_by_ask_sec: u64, pub reply_by_handoff_sec: u64 }
impl Default for Limits {
    fn default() -> Self { Limits { max_hops: 8, per_pair: 20, per_sender: 60, max_agent_runs: 3, loop_window: 3, loop_similarity: 0.9, reply_by_ask_sec: 1800, reply_by_handoff_sec: 86_400 } }
}
impl Limits {
    /// From the config's `agents` object (all optional).
    pub fn from_config(c: &Value) -> Limits {
        let d = Limits::default();
        let n = |k: &str, dv: u64| c[k].as_u64().filter(|x| *x > 0).unwrap_or(dv);
        Limits { max_hops: n("maxHops", d.max_hops), per_pair: n("perPairPerHour", d.per_pair as u64) as usize, per_sender: n("perSenderPerHour", d.per_sender as u64) as usize,
            max_agent_runs: n("maxAgentRuns", d.max_agent_runs as u64) as usize, loop_window: n("loopWindow", d.loop_window as u64) as usize,
            loop_similarity: c["loopSimilarity"].as_f64().filter(|x| *x > 0.0 && *x <= 1.0).unwrap_or(d.loop_similarity),
            reply_by_ask_sec: n("replyByAskSec", d.reply_by_ask_sec), reply_by_handoff_sec: n("replyByHandoffSec", d.reply_by_handoff_sec) }
    }
}
fn limits(hub: &dyn AiHub) -> Limits { Limits::from_config(hub.cfg().get("agents").unwrap_or(&Value::Null)) }

// ---------- the address and the resolver (section 2) ----------
/// `@gee`, `gee`, `@gee#s-1a2b3c4d5e6f` (one hub session), `@gee/jcode` (their Jcode PGM).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Address { pub user: String, pub session: String, pub source: String }
pub fn parse_address(to: &str) -> Option<Address> {
    let t = to.trim().trim_start_matches('@').trim().to_lowercase();
    let (user, session, source) = if let Some((u, x)) = t.split_once('#') { (u, x, "") } else if let Some((u, x)) = t.split_once('/') { (u, "", x) } else { (t.as_str(), "", "") };
    if !pgm_identity_internal::valid_username(user) { return None; }
    if !session.is_empty() && !(session.len() == 14 && session.starts_with("s-") && session[2..].bytes().all(|b| b.is_ascii_hexdigit())) { return None; }
    if !source.is_empty() && !(source.len() <= 24 && source.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')) { return None; }
    let source = if source == "work" { "" } else { source };
    Some(Address { user: user.into(), session: session.into(), source: source.into() })
}
/// Where a message to a person goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// one of their hub sessions (its id)
    Hub(String),
    /// a session another tool reports (T-289): its source, device and id
    External { source: String, device: String, id: String },
    /// none yet: their first session (made from what they had, as multi-session does)
    Primary,
}
fn usable(index: &Value, sid: &str) -> bool { let m = &index["sessions"][sid]; m.is_object() && m["archived"] != json!(true) && m["hidden"] != json!(true) }
fn ext_pick(external: &[Value], source: Option<&str>) -> Option<Target> {
    let mine: Vec<&Value> = external.iter().filter(|x| source.map(|s0| x["source"] == json!(s0)).unwrap_or(true)).collect();
    let newest = |l: &[&Value]| l.iter().max_by_key(|x| x["updatedAt"].as_u64().unwrap_or(0)).map(|x| (*x).clone());
    let busy: Vec<&Value> = mine.iter().copied().filter(|x| x["state"] == json!("working") || x["state"] == json!("waiting")).collect();
    newest(&busy).or_else(|| newest(&mine)).map(|x| Target::External { source: s(&x, "source"), device: s(&x, "device"), id: s(&x, "id") })
}
/// Section 2: the session a message to `addr` goes to, at the moment it is delivered. `index` = their work-sessions.json (Null
/// when they have none), `sticky` = the session the thread sticks to on their side ("" = none), `external` = the sessions
/// other tools report for them. None = the address names a session that is not there (any more).
pub fn resolve(index: &Value, addr: &Address, sticky: &str, external: &[Value]) -> Option<Target> {
    // 1. a session named in the address wins; then the session the thread sticks to (while it exists and is not archived)
    if !addr.session.is_empty() { let m = &index["sessions"][&addr.session]; return if m.is_object() && m["archived"] != json!(true) { Some(Target::Hub(addr.session.clone())) } else { None }; }
    if !addr.source.is_empty() { return ext_pick(external, Some(&addr.source)); }
    if !sticky.is_empty() { let m = &index["sessions"][sticky]; if m.is_object() && m["archived"] != json!(true) { return Some(Target::Hub(sticky.to_string())); } }
    // 2. the current session (an archived or hidden current counts as none)
    let cur = s(index, "current");
    if !cur.is_empty() && usable(index, &cur) { return Some(Target::Hub(cur)); }
    // 3. no hub sessions at all but external ones: the newest working / waiting one, else the newest
    let none = index["sessions"].as_object().map(|o| o.is_empty()).unwrap_or(true);
    if none { if let Some(t) = ext_pick(external, None) { return Some(t); } return Some(Target::Primary); }
    // 4. the primary (first) session; one that is usable when it is hidden or archived; else the primary all the same
    let pri = s(index, "primary");
    if !pri.is_empty() && usable(index, &pri) { return Some(Target::Hub(pri)); }
    if let Some(sid) = index["order"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).find(|x| usable(index, x)) { return Some(Target::Hub(sid.to_string())); }
    if !pri.is_empty() && index["sessions"][&pri].is_object() { return Some(Target::Hub(pri)); }
    Some(Target::Primary)
}

// ---------- the loop detector and the rate limits (section 8) ----------
/// A text's words, normalised (lower case, letters and digits only).
pub fn words(t: &str) -> HashSet<String> { t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect() }
/// Word-set Jaccard similarity (two empty texts are the same).
pub fn jaccard(a: &str, b: &str) -> f64 {
    let (x, y) = (words(a), words(b));
    if x.is_empty() && y.is_empty() { return 1.0; }
    x.intersection(&y).count() as f64 / x.union(&y).count() as f64
}
fn ref_keys(refs: &Value) -> Vec<String> { refs.as_object().map(|o| o.iter().filter(|(_, v)| v.as_str().is_some_and(|x| !x.is_empty())).map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect()).unwrap_or_default() }
/// Are the last `window` messages of a thread (sender, text, refs; the new one last) going in circles? Each of them alternates
/// sender (A→B→A), brings no ref the thread did not have, and is at least `similarity` like the one two steps back.
pub fn is_loop(msgs: &[(String, String, Value)], window: usize, similarity: f64) -> bool {
    let n = msgs.len();
    if window == 0 || n < window + 2 { return false; }
    let mut seen: HashSet<String> = msgs[..n - window].iter().flat_map(|m| ref_keys(&m.2)).collect();
    for i in n - window..n {
        if msgs[i].0 == msgs[i - 1].0 { return false; }
        if jaccard(&msgs[i].1, &msgs[i - 2].1) < similarity { return false; }
        for r in ref_keys(&msgs[i].2) { if !seen.insert(r) { continue; } return false; }
    }
    true
}
/// The messages sent in the last hour (who to whom), counted on the hub.
#[derive(Default, Debug)]
pub struct RateLog { sent: Vec<(u64, String, String)> }
impl RateLog {
    /// May `from` send one more to `to` now? Err = the sentence the sender is told.
    pub fn check(&mut self, now: u64, from: &str, to: &str, l: &Limits) -> Result<(), String> {
        self.sent.retain(|x| now.saturating_sub(x.0) < 3_600_000);
        if self.sent.iter().filter(|x| x.1 == from && x.2 == to).count() >= l.per_pair { return Err(format!("slow down: {} messages an hour between you and @{to}", l.per_pair)); }
        if self.sent.iter().filter(|x| x.1 == from).count() >= l.per_sender { return Err(format!("slow down: {} agent messages an hour from you", l.per_sender)); }
        Ok(())
    }
    pub fn note(&mut self, now: u64, from: &str, to: &str) { self.sent.push((now, from.to_string(), to.to_string())); }
}

// ---------- settings and threads (section 9) ----------
fn agents_dir(a: &Account) -> PathBuf { a.dir.join("agents") }
fn settings_file(a: &Account) -> PathBuf { agents_dir(a).join("settings.json") }
fn thread_dir(a: &Account) -> PathBuf { agents_dir(a).join("threads") }
fn valid_thread(id: &str) -> bool { id.len() == 15 && id.starts_with("at-") && id[3..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) }
fn thread_file(a: &Account, id: &str) -> Option<PathBuf> { if valid_thread(id) { Some(thread_dir(a).join(format!("{id}.json"))) } else { None } }
fn load_thread(a: &Account, id: &str) -> Option<Value> { thread_file(a, id).and_then(|f| read_json(&f)).filter(|t| t["messages"].is_array()) }
fn save_thread(a: &Account, id: &str, t: &Value) { if let Some(f) = thread_file(a, id) { let _ = std::fs::create_dir_all(thread_dir(a)); write_json(&f, t); } }
fn all_threads(a: &Account) -> Vec<Value> {
    std::fs::read_dir(thread_dir(a)).map(|r| r.filter_map(|e| e.ok()).filter_map(|e| { let n = e.file_name().to_string_lossy().to_string(); n.strip_suffix(".json").filter(|id| valid_thread(id)).and_then(|id| load_thread(a, id)) }).collect()).unwrap_or_default()
}
/// The person's settings (defaults filled in: nobody muted, not paused, ask first for a hand-off, wake me).
pub fn settings_of(a: &Account) -> Value { norm_settings(&read_json(&settings_file(a)).unwrap_or(Value::Null)) }
fn norm_settings(v: &Value) -> Value {
    let list = |x: &Value| -> Vec<String> { x.as_array().map(|l| l.iter().filter_map(|u| u.as_str().map(|u| u.to_lowercase())).collect()).unwrap_or_default() };
    let ask: Vec<String> = if v["askFirst"].is_array() { list(&v["askFirst"]).into_iter().filter(|k| HOLDABLE.contains(&k.as_str())).collect() } else { vec!["handoff".into()] };
    json!({ "muted": list(&v["muted"]), "paused": v["paused"] == json!(true), "askFirst": ask, "wake": v["wake"] != json!(false) })
}
/// A message as the envelope says it (the hub's own notes left out).
pub fn envelope(m: &Value) -> Value { let mut o = Map::new(); for k in ENVELOPE { o.insert(k.into(), m.get(k).cloned().unwrap_or(Value::Null)); } Value::Object(o) }
fn cut_text(t: &str) -> (String, bool) {
    let n = t.chars().count();
    if n <= TEXT_MAX { return (t.to_string(), false); }
    (t.chars().take(TEXT_MAX - 1).collect::<String>() + "…", true)
}
fn clean_refs(v: &Value) -> Value {
    let mut o = Map::new();
    for k in ["task", "note", "commit", "branch", "project", "run", "url"] { if let Some(x) = v[k].as_str().map(|x| x.trim()).filter(|x| !x.is_empty()) { o.insert(k.into(), json!(x.chars().filter(|c| !c.is_control()).take(300).collect::<String>())); } }
    Value::Object(o)
}
fn subject_of(t: &str) -> String { t.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(120).collect() }
fn hhmm(ms: u64) -> String { chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default() }
fn accepted(m: &Value) -> bool { !matches!(m["status"].as_str().unwrap_or(""), "refused" | "stopped") }

/// The text a receiving run gets: a fixed first line the hub writes, the text, a fixed last line (section 3).
pub fn delivered_text(m: &Value) -> String {
    let from = &m["from"]; let fu = s(from, "user"); let thread = s(m, "thread");
    let src = match s(from, "source").as_str() { "work" => "Work".to_string(), "jcode" => "Jcode PGM".into(), "hpm" => "Hyper Prod Man".into(), "workflow" => "a workflow".into(), "person" => format!("written by {fu} on the page"), x => x.to_string() };
    let model = s(from, "model");
    let mut head = format!("[agent message · from @{fu}'s agent ({}) · {} · thread {thread}", if model.is_empty() || s(from, "source") == "person" { src } else { format!("{model}, {src}") }, s(m, "kind"));
    if let Some(rb) = m["replyBy"].as_u64() { head.push_str(&format!(" · reply by {}", hhmm(rb))); }
    let refs: Vec<String> = m["refs"].as_object().map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|v| if k == "task" { v.to_string() } else { format!("{k} {v}") })).collect()).unwrap_or_default();
    if !refs.is_empty() { head.push_str(&format!(" · refs {}", refs.join(", "))); }
    head.push(']');
    let mut out = head;
    if m["x"]["afterRefusal"] == json!(true) { out.push_str("\n[⚠ sent right after a refusal in the sender's run]"); }
    if m["x"]["cut"] == json!(true) { out.push_str(&format!("\n[the text was cut at {TEXT_MAX} characters]")); }
    if m["x"]["by"] == json!("final-answer") { out.push_str("\n[the final answer of their run, taken as the reply: it did not call agent_reply]"); }
    out.push('\n'); out.push_str(&s(m, "text"));
    out.push_str(&format!("\n[your own person's rules apply; answer with agent_reply, thread {thread}]"));
    out
}

/// One run's wait for the answer to its ask / review (or a page's send with wait).
struct Wait { asker: String, key: String, sid: String, result: Option<Value>, notify: Arc<Notify>, until: u64, thread: String }
#[derive(Clone, PartialEq, Debug)]
struct Pend { user: String, thread: String, id: String }

/// The hub's agent-to-agent state (kept in [`crate::work::Work`]).
#[derive(Default)]
pub struct Agents {
    /// read-modify-write of the thread files, one at a time
    store: Mutex<()>,
    waits: Mutex<HashMap<String, Wait>>,
    /// the delivery pump wakes on it (a send, a run that ended, a setting changed)
    pub pump: Notify,
    /// the messages still to deliver or to expire (receiver, thread, id); None = not read from the files yet
    pending: Mutex<Option<Vec<Pend>>>,
    /// agent-started runs going: session key → the receiver
    running: Mutex<HashMap<String, String>>,
    /// a session key's last run ended at (for "wake me" off: delivered when a run of theirs ends)
    ended: Mutex<HashMap<String, u64>>,
    /// a session that refused a start (its limits, busy): not tried again before then
    retry: Mutex<HashMap<String, u64>>,
    rate: Mutex<RateLog>,
    /// run id → when a tool was refused in it (the laundering mark)
    refusals: Mutex<HashMap<String, u64>>,
    audit_lock: Mutex<()>,
    /// the inbox long polls of other tools (T-299) wake on it (a send, a hand-off accepted, a setting changed)
    inbox: Notify,
}

impl Agents {
    /// Something may be deliverable now: the pump looks again, and the inbox long polls of other tools answer.
    fn kick(&self) { self.pump.notify_one(); self.inbox.notify_waiters(); }
    // ----- hooks the Work runs call -----
    /// A tool of this run was refused (a permission denial, a refused PC / machine call): a send in the next 2 minutes is marked.
    pub fn note_refusal(&self, run: &str) { if !run.is_empty() { let now = now_ms(); let mut m = lk(&self.refusals); m.retain(|_, at| now.saturating_sub(*at) < 10 * 60_000); m.insert(run.to_string(), now); } }
    fn after_refusal(&self, run: &str, now: u64) -> bool { !run.is_empty() && lk(&self.refusals).get(run).is_some_and(|at| now.saturating_sub(*at) <= AFTER_REFUSAL_MS) }
    /// A run of this session ended: its waits end with it (the answer then comes as a message), the pump looks again.
    /// True when a wait of that run ended with it (its session's dot stops waiting).
    pub fn run_ended(&self, key: &str) -> bool {
        lk(&self.ended).insert(key.to_string(), now_ms());
        lk(&self.retry).remove(key);
        let gone: Vec<String> = lk(&self.waits).iter().filter(|(_, w)| w.key == key && !key.is_empty() && w.result.is_none()).map(|(id, _)| id.clone()).collect();
        let any = !gone.is_empty();
        for id in gone { if let Some(w) = lk(&self.waits).remove(&id) { w.notify.notify_waiters(); } }
        self.pump.notify_one();
        any
    }

    // ----- the store -----
    /// Read-modify-write of one thread, both people's copies in one go (two copies, one truth). `f` gets the messages (merged
    /// from both copies, in order) and the shared fields; `make` = the shared fields of a new thread (None: it must exist).
    fn with_thread<T>(&self, hub: &dyn AiHub, a: &str, b: &str, id: &str, make: Option<Value>, f: impl FnOnce(&mut Vec<Value>, &mut Map<String, Value>) -> T) -> Option<T> {
        let _g = lk(&self.store);
        let (Some(aa), Some(ab)) = (hub.account(a), hub.account(b)) else { return None };
        let (ca, cb) = (load_thread(&aa, id), load_thread(&ab, id));
        if ca.is_none() && cb.is_none() && make.is_none() { return None; }
        let mut msgs: Vec<Value> = vec![];
        for c in [&ca, &cb].into_iter().flatten() { for m in c["messages"].as_array().into_iter().flatten() { if !msgs.iter().any(|x| x["id"] == m["id"]) { msgs.push(m.clone()); } } }
        msgs.sort_by_key(|m| m["at"].as_u64().unwrap_or(0));
        let base = ca.clone().or_else(|| cb.clone()).or(make).unwrap_or(json!({}));
        let mut shared = Map::new(); for k in SHARED { shared.insert(k.into(), base.get(k).cloned().unwrap_or(Value::Null)); }
        let out = f(&mut msgs, &mut shared);
        for (acc, copy, other) in [(&aa, &ca, b), (&ab, &cb, a)] {
            let mine: Vec<Value> = msgs.iter().filter(|m| { let only = s(&m["x"], "only"); only.is_empty() || only == acc.username }).cloned().collect();
            if copy.is_none() && mine.is_empty() { continue; }
            let mut t = copy.clone().unwrap_or_else(|| json!({ "id": id, "with": other, "session": "", "seenAt": 0 }));
            for (k, v) in &shared { t[k] = v.clone(); }
            t["messages"] = json!(mine);
            save_thread(acc, id, &t);
        }
        Some(out)
    }
    /// One side's own fields of a thread (the session it sticks to, when the person last looked).
    fn with_side(&self, a: &Account, id: &str, f: impl FnOnce(&mut Value)) { let _g = lk(&self.store); if let Some(mut t) = load_thread(a, id) { f(&mut t); save_thread(a, id, &t); } }
    /// Changes one message (both copies).
    fn set_msg(&self, hub: &dyn AiHub, m: &Value, f: impl FnOnce(&mut Value)) -> Option<Value> {
        let (a, b, t, id) = (s(&m["from"], "user"), s(&m["to"], "user"), s(m, "thread"), s(m, "id"));
        self.with_thread(hub, &a, &b, &t, None, |msgs, _| { let x = msgs.iter_mut().find(|x| x["id"] == json!(id))?; f(x); Some(x.clone()) }).flatten()
    }
    fn find_msg(a: &Account, id: &str) -> Option<Value> { all_threads(a).into_iter().find_map(|t| t["messages"].as_array().and_then(|l| l.iter().find(|m| m["id"] == json!(id)).cloned())) }

    // ----- the audit (Chat/agent-audit.ndjson, rotated monthly, 12 months kept) -----
    fn audit_file(hub: &dyn AiHub) -> PathBuf { hub.mills_dir().join("Chat").join("agent-audit.ndjson") }
    fn audit(&self, hub: &dyn AiHub, event: &str, m: &Value, run: &str, why: &str) {
        let _g = lk(&self.audit_lock);
        let f = Self::audit_file(hub);
        let month = |ms: u64| chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.format("%Y-%m").to_string()).unwrap_or_default();
        let now = now_ms();
        if let Some(old) = std::fs::metadata(&f).ok().and_then(|x| x.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| month(d.as_millis() as u64)) {
            if old != month(now) {
                pgm_core_internal::store::global().flush();
                let _ = std::fs::rename(&f, f.with_file_name(format!("agent-audit-{old}.ndjson"))); pgm_core_internal::store::forget_hot(&f);
                let keep = month(now.saturating_sub(366 * 86_400_000));
                if let Ok(r) = std::fs::read_dir(f.parent().unwrap_or(&f)) { for e in r.filter_map(|e| e.ok()) { let n = e.file_name().to_string_lossy().to_string(); if let Some(mo) = n.strip_prefix("agent-audit-").and_then(|x| x.strip_suffix(".ndjson")) { if mo < keep.as_str() { let _ = std::fs::remove_file(e.path()); } } } }
            }
        }
        let line = json!({ "at": now, "event": event, "id": m["id"], "thread": m["thread"], "from": m["from"], "to": m["to"], "kind": m["kind"], "refs": m["refs"], "run": run, "why": why, "afterRefusal": m["x"]["afterRefusal"] == json!(true) });
        let mut line = line; if m["x"]["by"].is_string() { line["by"] = m["x"]["by"].clone(); }
        let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
        pgm_core_internal::store::global().append_line(&f, &line);
    }
    fn audit_lines(hub: &dyn AiHub, since: u64) -> Vec<Value> {
        let f = Self::audit_file(hub);
        let mut files: Vec<PathBuf> = std::fs::read_dir(f.parent().unwrap_or(&f)).map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.file_name().map(|n| { let n = n.to_string_lossy(); n.starts_with("agent-audit-") && n.ends_with(".ndjson") }).unwrap_or(false)).collect()).unwrap_or_default();
        files.sort(); files.push(f);
        let since_month = chrono::DateTime::from_timestamp_millis(since as i64).map(|d| d.format("%Y-%m").to_string()).unwrap_or_default();
        files.into_iter().filter(|p| { let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(); n.strip_prefix("agent-audit-").and_then(|x| x.strip_suffix(".ndjson")).map(|mo| mo >= since_month.as_str()).unwrap_or(true) })
            .flat_map(|p| pgm_core_internal::store::read_lines_hot(&p)).filter(|l| l["at"].as_u64().unwrap_or(0) >= since).collect()
    }

    // ----- telling people (a private chat line, only for: a held message, a stopped loop, an expired ask) -----
    fn tell(hub: &dyn AiHub, to: &str, text: &str) { if hub.chat_has_key(to) { hub.agent_say(text, to, "VibeRoller", "", ""); } }

    // ----- sending -----
    /// A message from `from` (an envelope's from: user, session, source, model, run): POST /api/agents/send and the send tools
    /// (`reply` = POST /api/agents/reply and agent_reply / agent_report: it names its thread and answers the other side's last
    /// message). `wait` = (session key, session id) of a sender that will wait for the answer: its wait is set up before the
    /// message can be delivered. Answers the HTTP status and body of section 6.
    pub fn send(&self, hub: &dyn AiHub, from: &Value, b: &Value, reply: bool, wait: Option<(String, String)>) -> (u16, Value) { self.send_by(hub, from, b, reply, wait, "") }
    /// Where a message for another tool goes, decided when it is sent (it then waits in that tool's inbox, section 4): a named
    /// tool (`@gee/jcode`: its newest session, else just that tool's inbox), or the external session the receiver's side of the
    /// thread came from while it still reports (replies go back to the session that asked). None = a hub session (the pump
    /// resolves it at delivery, section 2, and also finds rule 3's external sessions then). (source, device, session id)
    fn external_to(ra: &Account, addr: &Address, tid: &str, now: u64) -> Option<(String, String, String)> {
        if !addr.session.is_empty() { return None; }
        let ext = sessions::ext_live(&ra.username, now);
        if !addr.source.is_empty() {
            return Some(match ext_pick(&ext, Some(&addr.source)) { Some(Target::External { source, device, id }) => (source, device, id), _ => (addr.source.clone(), String::new(), String::new()) });
        }
        let t = load_thread(ra, tid)?;
        let (src, sid) = (s(&t, "source"), s(&t, "session"));
        if src.is_empty() || src == "work" { return None; }
        ext.iter().find(|x| s(x, "source") == src && s(x, "id") == sid).map(|x| (src.clone(), s(x, "device"), sid.clone()))
    }
    /// [`Agents::send`], with the hub's own mark of who wrote it (`by`: "final-answer" for a run's final answer taken as the reply).
    fn send_by(&self, hub: &dyn AiHub, from: &Value, b: &Value, reply: bool, wait: Option<(String, String)>, by: &str) -> (u16, Value) {
        let lim = limits(hub);
        let me = s(from, "user");
        let bad = |e: String| (400, json!({ "error": e, "why": "bad" }));
        let kind = { let k = s(b, "kind"); if k.is_empty() { if reply { "reply".to_string() } else { "ask".into() } } else { k } };
        if !KINDS.contains(&kind.as_str()) { return bad(format!("Unknown kind \"{}\" (ask, handoff, review, report or reply).", kind.chars().take(20).collect::<String>())); }
        if reply && !matches!(kind.as_str(), "reply" | "report") { return bad("An answer in a thread is a reply or a report.".into()); }
        let raw = s(b, "text");
        if raw.trim().is_empty() { return bad("An agent message needs its text.".into()); }
        let (text, cut) = cut_text(&raw);
        let refs = clean_refs(&b["refs"]);
        if kind == "handoff" && s(&refs, "task").is_empty() { return bad("A hand-off names the board task it hands off (refs.task, e.g. T-12).".into()); }
        let tin = s(b, "thread");
        let Some(sa) = hub.account(&me) else { return bad("Unknown account.".into()) };
        // the thread (a reply names one; a send may) and who it goes to
        let (addr, existing) = if !tin.is_empty() {
            let Some(t) = load_thread(&sa, &tin) else { return (404, json!({ "error": "No such thread of yours.", "why": "bad" })) };
            let with = s(&t, "with");
            let addr = if s(b, "to").is_empty() { Address { user: with.clone(), ..Default::default() } } else { match parse_address(&s(b, "to")) { Some(x) => x, None => return bad(format!("\"{}\" is not an agent address (@username).", s(b, "to").chars().take(40).collect::<String>())) } };
            if addr.user != with { return bad(format!("That thread is with @{with}.")); }
            (addr, Some(t))
        } else {
            if reply { return bad("A reply names its thread.".into()); }
            match parse_address(&s(b, "to")) { Some(x) => (x, None), None => return bad(format!("\"{}\" is not an agent address (@username).", s(b, "to").chars().take(40).collect::<String>())) }
        };
        let to = addr.user.clone();
        let Some(ra) = hub.account(&to) else { return bad(format!("No account called \"{to}\" (agents_list names the team).")) };
        if to == me { return bad("That is you: your own agent is this one.".into()); }
        let set = settings_of(&ra);
        let now = now_ms();
        let id = format!("am-{}", pgm_core_internal::crypto::random_hex(6));
        let tid = if tin.is_empty() { format!("at-{}", pgm_core_internal::crypto::random_hex(6)) } else { tin.clone() };
        let rb_sec = match kind.as_str() { "ask" | "review" => Some(b["replyBySec"].as_u64().filter(|x| *x > 0).map(|x| x.min(7 * 86_400)).unwrap_or(lim.reply_by_ask_sec)), "handoff" => Some(lim.reply_by_handoff_sec), _ => None };
        let ext_to = Self::external_to(&ra, &addr, &tid, now);
        let (to_source, to_session) = match &ext_to { Some((src, _, sid)) => (src.clone(), sid.clone()), None => ("work".to_string(), String::new()) };
        let mut m = json!({ "id": id, "thread": tid, "from": from, "to": { "user": to, "session": to_session, "source": to_source },
            "kind": kind, "text": text, "refs": refs, "hop": 1, "parent": "", "replyBy": rb_sec.map(|x| json!(now + x * 1000)).unwrap_or(Value::Null), "at": now, "status": "queued", "why": "",
            "x": { "afterRefusal": self.after_refusal(&s(from, "run"), now) } });
        if let Some((_, dev, _)) = &ext_to { if !dev.is_empty() { m["x"]["device"] = json!(dev); } }
        // the sender waits for the answer: a run whose model never calls agent_reply has its final answer taken as the reply (§ 7)
        if wait.is_some() && matches!(kind.as_str(), "ask" | "review") { m["x"]["wait"] = json!(true); }
        if !by.is_empty() { m["x"]["by"] = json!(by); }
        let want_parent = s(b, "parent");
        if cut { m["x"]["cut"] = json!(true); }
        if !addr.session.is_empty() { m["x"]["session"] = json!(addr.session); }
        let make = existing.as_ref().map(|_| None).unwrap_or_else(|| Some(json!({ "subject": subject_of(&text), "status": "open", "why": "", "createdAt": now, "lastAt": now, "hopReset": 0 })));
        let name = hub.name_of(&to);
        // muted: refused, nothing queued; the attempt is shown to the muter only
        if set["muted"].as_array().is_some_and(|l| l.iter().any(|u| u == &json!(me))) {
            m["status"] = json!("refused"); m["why"] = json!("muted"); m["x"]["only"] = json!(to);
            let mm = m.clone();
            self.with_thread(hub, &me, &to, &tid, make, |msgs, sh| { msgs.push(mm); sh.insert("lastAt".into(), json!(now)); });
            self.audit(hub, "refused", &m, &s(from, "run"), "muted");
            return (403, json!({ "error": format!("{name}'s agent is not taking messages from yours."), "why": "muted", "id": id, "thread": tid }));
        }
        if let Err(e) = lk(&self.rate).check(now, &me, &to, &lim) { self.audit(hub, "refused", &m, &s(from, "run"), "rate"); return (429, json!({ "error": e, "why": "rate" })); }
        let hold = !reply && set["askFirst"].as_array().is_some_and(|l| l.iter().any(|k| k == &json!(kind)));
        // into the thread: stopped / hops / loop / full first
        let mm = m.clone(); let (win, sim, maxh) = (lim.loop_window, lim.loop_similarity, lim.max_hops);
        let by_person = s(from, "source") == "person";
        let res = self.with_thread(hub, &me, &to, &tid, make, |msgs, sh| -> Result<(Value, String), (u16, &'static str, Value)> {
            let mut m = mm;
            // a person's own message in a thread their agents' loop stopped reopens it (section 8)
            if by_person && sh.get("status") == Some(&json!("stopped")) && sh.get("why") == Some(&json!("loop")) {
                sh.insert("status".into(), json!("open")); sh.insert("why".into(), json!(""));
                sh.insert("hopReset".into(), json!(msgs.iter().filter(|x| accepted(x)).filter_map(|x| x["hop"].as_u64()).max().unwrap_or(0)));
            }
            if sh.get("status") == Some(&json!("stopped")) { let why = sh.get("why").and_then(|x| x.as_str()).unwrap_or("stopped").to_string(); return Err((409, "stopped", json!(why))); }
            let last_hop = msgs.iter().filter(|x| accepted(x)).filter_map(|x| x["hop"].as_u64()).max().unwrap_or(0);
            m["hop"] = json!(last_hop + 1);
            let mut parent = String::new();
            // a reply answers the other side's message it names (one of theirs in this thread), else their last one
            if reply {
                let named = msgs.iter().find(|x| !want_parent.is_empty() && x["id"] == json!(want_parent) && s(&x["from"], "user") == to);
                if let Some(p) = named.or_else(|| msgs.iter().rev().find(|x| s(&x["from"], "user") == to)) { parent = s(p, "id"); }
            }
            m["parent"] = json!(parent);
            let reset = sh.get("hopReset").and_then(Value::as_u64).unwrap_or(0);
            let stop = |sh: &mut Map<String, Value>, why: &str| { sh.insert("status".into(), json!("stopped")); sh.insert("why".into(), json!(why)); sh.insert("lastAt".into(), json!(now)); };
            if last_hop + 1 > reset + maxh { m["status"] = json!("refused"); m["why"] = json!("hops"); msgs.push(m.clone()); stop(sh, "hops"); return Err((429, "hops", m)); }
            let mut seq: Vec<(String, String, Value)> = msgs.iter().filter(|x| accepted(x)).map(|x| (s(&x["from"], "user"), s(x, "text"), x["refs"].clone())).collect();
            seq.push((me.clone(), s(&m, "text"), m["refs"].clone()));
            if is_loop(&seq, win, sim) { m["status"] = json!("stopped"); m["why"] = json!("loop"); msgs.push(m.clone()); stop(sh, "loop"); return Err((409, "loop", m)); }
            if msgs.len() + 1 >= THREAD_MAX { m["status"] = json!("stopped"); m["why"] = json!("full"); msgs.push(m.clone()); stop(sh, "full"); return Err((409, "full", m)); }
            if hold { m["status"] = json!("held"); }
            if !parent.is_empty() { if let Some(p) = msgs.iter_mut().find(|x| x["id"] == json!(parent)) { if matches!(p["status"].as_str().unwrap_or(""), "queued" | "delivered" | "read") { p["status"] = json!("answered"); } } }
            msgs.push(m.clone());
            sh.insert("lastAt".into(), json!(now));
            Ok((m, parent))
        });
        let Some(res) = res else { return bad("Unknown account.".into()) };
        let run = s(from, "run");
        let (m, parent) = match res {
            Ok(x) => x,
            Err((st, why, mx)) => {
                self.audit(hub, if why == "stopped" { "refused" } else { "stopped" }, if mx.is_object() { &mx } else { &m }, &run, why);
                let subj = existing.as_ref().map(|t| s(t, "subject")).filter(|x| !x.is_empty()).unwrap_or_else(|| subject_of(&text));
                let about = s(&refs, "task"); let about = if about.is_empty() { format!("\"{subj}\"") } else { about };
                let n = load_thread(&sa, &tid).and_then(|t| t["messages"].as_array().map(|l| l.len())).unwrap_or(0);
                let error = match why {
                    "loop" => { let line = format!("Your agents were going in circles about {about}; stopped after {n} messages; open the thread to continue."); Self::tell(hub, &me, &line); Self::tell(hub, &to, &line); format!("Stopped: the agents were going in circles in this thread (no progress in the last {win} exchanges). Tell {} instead of trying again.", hub.name_of(&me)) }
                    "hops" => { let line = format!("The agents' thread about {about} reached {maxh} messages and was stopped; open the thread to continue."); Self::tell(hub, &me, &line); Self::tell(hub, &to, &line); format!("Stopped: the thread reached its limit of {maxh} hops. Tell {} instead of trying again.", hub.name_of(&me)) }
                    "full" => format!("Stopped: the thread is full ({THREAD_MAX} messages). Start a new one."),
                    _ => format!("This thread is stopped ({}); either person can reopen it.", mx.as_str().unwrap_or("stopped")),
                };
                return (st, json!({ "error": error, "why": why, "id": id, "thread": tid }));
            }
        };
        lk(&self.rate).note(now, &me, &to);
        // the session the thread sticks to on the sender's side: the one that sent it (a run's), the first time
        // (another tool's session, T-299: with its source, so the answer goes back into that tool's inbox)
        let (from_sid, from_src) = (s(from, "session"), s(from, "source"));
        if !from_sid.is_empty() { self.with_side(&sa, &tid, |t| { if s(t, "session").is_empty() { t["session"] = json!(from_sid); if !matches!(from_src.as_str(), "work" | "person" | "") { t["source"] = json!(from_src); } } }); }
        self.audit(hub, "sent", &m, &run, "");
        let held = m["status"] == json!("held");
        if held {
            self.audit(hub, "held", &m, &run, "");
            let what = if kind == "handoff" { format!("to take {}", s(&refs, "task")) } else { format!("for a {kind}: \"{}\"", subject_of(&text)) };
            Self::tell(hub, &to, &format!("{}'s agent asks you {what} · Accept / Decline in Work", hub.name_of(&me)));
        }
        // the answer to a sender that waits for it reaches that wait (and the asking session is not woken for it)
        let mut status = s(&m, "status");
        if !parent.is_empty() && self.settle(hub, &parent, &m) { status = "read".into(); }
        else if let Some((key, sid)) = wait.filter(|_| !held) {
            let until = m["replyBy"].as_u64().unwrap_or(now + WAIT_MAX_MS).min(now + WAIT_MAX_MS);
            lk(&self.waits).insert(id.clone(), Wait { asker: me.clone(), key, sid, result: None, notify: Arc::new(Notify::new()), until, thread: tid.clone() });
        }
        if status != "read" { self.add_pending(Pend { user: to.clone(), thread: tid.clone(), id: id.clone() }); }
        self.kick();
        let note = if held { format!("held for {name}'s okay") } else if set["paused"] == json!(true) && !(reply && !parent.is_empty()) { format!("queued: {name} paused their inbox") } else if status == "read" { "answered the waiting sender".to_string() } else { String::new() };
        (202, json!({ "id": id, "thread": tid, "to": m["to"], "status": status, "note": note }))
    }
    fn add_pending(&self, p: Pend) { let mut g = lk(&self.pending); let l = g.get_or_insert_with(Vec::new); if !l.contains(&p) { l.push(p); } }
    /// The answer `reply` came for `parent`: when someone waits for it, the wait ends with it. True = it was waited for.
    fn settle(&self, hub: &dyn AiHub, parent: &str, reply: &Value) -> bool {
        let w = { let mut g = lk(&self.waits); match g.get_mut(parent) { Some(w) if w.result.is_none() => { w.result = Some(json!({ "id": parent, "thread": reply["thread"], "reply": envelope(reply), "text": reply["text"], "from": reply["from"]["user"], "by": if reply["x"]["by"].is_string() { reply["x"]["by"].clone() } else { json!("agent") } })); w.notify.notify_waiters(); Some((w.key.clone(), w.asker.clone())) } _ => None } };
        let Some((key, _asker)) = w else { return false };
        let mut r = reply.clone(); r["status"] = json!("read");
        self.set_msg(hub, reply, |x| { x["status"] = json!("read"); });
        self.audit(hub, "replied", &r, &s(&reply["from"], "run"), "");
        if !key.is_empty() { hub.work().emit_key(&key, json!({ "type": "agent_reply", "message": envelope(&r) })); }
        true
    }
    /// Waits for the answer to `id` (a send with wait) up to `poll` seconds: Some(the answer) / Some(no longer waited: it comes
    /// later as a message) / None (still waiting: the caller answers {pending}).
    pub async fn wait_for(&self, hub: &dyn AiHub, user: &str, id: &str, poll: u64) -> Value {
        let end = std::time::Instant::now() + Duration::from_secs(poll);
        let mut marked = false;
        loop {
            let snap = lk(&self.waits).get(id).filter(|w| w.asker == user).map(|w| (w.notify.clone(), w.result.clone(), w.key.clone(), w.sid.clone(), w.until, w.thread.clone()));
            let Some((notify, res, key, sid, until, thread)) = snap else { return json!({ "sent": true, "pending": true, "id": id, "note": "Nobody waits for that answer any more: it comes as a message into the session that asked." }) };
            let nf = notify.notified(); tokio::pin!(nf); nf.as_mut().enable();
            if let Some(r) = res { lk(&self.waits).remove(id); self.waiting(hub, user, &sid, None); return r; }
            if !key.is_empty() { hub.work().keep_alive_key(&key); }
            if !marked { self.waiting(hub, user, &sid, Some(id)); marked = true; }
            let now = now_ms();
            if now >= until {
                lk(&self.waits).remove(id); self.waiting(hub, user, &sid, None);
                return json!({ "sent": true, "pending": true, "id": id, "thread": thread, "note": "No answer yet: it comes later as a message into this session." });
            }
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() { return json!({ "pending": true, "id": id }); }
            let _ = tokio::time::timeout(left.min(Duration::from_secs(2)).min(Duration::from_millis(until.saturating_sub(now).max(1))), nf).await;
        }
    }
    /// The asking session's dot: waiting (yellow) for another person's agent while one of its waits is open (section 4).
    fn waiting(&self, hub: &dyn AiHub, user: &str, sid: &str, id: Option<&str>) {
        if sid.is_empty() { return; }
        let Some(a) = hub.account(user) else { return };
        let open = id.is_some() || lk(&self.waits).values().any(|w| w.asker == user && w.sid == sid && w.result.is_none());
        sessions::touch(&a, sid, |m| {
            let mine = m.get("waiting").map(|w| w["kind"] == json!("agent")).unwrap_or(false);
            if open { if !m.get("waiting").is_some_and(Value::is_object) { m.insert("waiting".into(), json!({ "kind": "agent", "at": now_ms(), "ref": id.unwrap_or("") })); } }
            else if mine { m.remove("waiting"); }
        });
    }

    // ----- the person's controls -----
    /// POST /api/agents/held {id, accept, note?}: the "ask me first" card's answer.
    pub fn held(&self, hub: &dyn AiHub, a: &Account, b: &Value) -> (u16, Value) {
        let id = s(b, "id");
        let Some(m) = Self::find_msg(a, &id).filter(|m| s(&m["to"], "user") == a.username) else { return (404, json!({ "error": "No such message for you." })) };
        if m["status"] != json!("held") { return (409, json!({ "error": format!("That message is not waiting for your okay (it is {}).", s(&m, "status")), "status": m["status"] })); }
        let sender = s(&m["from"], "user");
        let note: String = s(b, "note").trim().chars().take(2000).collect();
        if b["accept"] != json!(true) {
            let nn = note.clone();
            let Some(m2) = self.set_msg(hub, &m, |x| { x["status"] = json!("refused"); x["why"] = json!("declined"); if !nn.is_empty() { x["x"]["note"] = json!(nn); } }) else { return (404, json!({ "error": "No such message for you." })) };
            self.audit(hub, "declined", &m2, "", "declined");
            // the sender's agent hears it (a report in the thread, from the person)
            let text = format!("{} declined this {}{}.{}", a.display, s(&m, "kind"), if s(&m["refs"], "task").is_empty() { String::new() } else { format!(" of {}", s(&m["refs"], "task")) }, if note.is_empty() { String::new() } else { format!(" Their note: {note}") });
            let from = json!({ "user": a.username, "session": "", "source": "person", "model": "", "run": "" });
            let _ = self.send(hub, &from, &json!({ "thread": s(&m, "thread"), "kind": "report", "text": text }), true, None);
            return (200, json!({ "ok": true, "status": "refused" }));
        }
        let Some(m2) = self.set_msg(hub, &m, |x| { x["status"] = json!("queued"); }) else { return (404, json!({ "error": "No such message for you." })) };
        self.audit(hub, "accepted", &m2, "", "");
        // accepting a hand-off re-assigns its task to the receiver, signed by the receiver: the only re-assign that happens
        let task = s(&m["refs"], "task");
        let (reassigned, why) = if m["kind"] == json!("handoff") && !task.is_empty() { Self::reassign(hub, &a.username, &sender, &task) } else { (false, String::new()) };
        if reassigned { self.audit(hub, "reassigned", &m2, "", ""); }
        self.add_pending(Pend { user: a.username.clone(), thread: s(&m, "thread"), id: id.clone() });
        self.kick();
        (200, json!({ "ok": true, "status": "queued", "reassigned": reassigned, "note": why }))
    }
    /// A hand-off of `task` accepted by `receiver`: the task becomes theirs when it was the sender's, nobody's or theirs already;
    /// a third person's task stays with them (their own say), and the answer says so.
    fn reassign(hub: &dyn AiHub, receiver: &str, sender: &str, task: &str) -> (bool, String) {
        let got = hub.board_tool("get", receiver, &json!({ "id": task }));
        if got["ok"] != json!(true) { return (false, format!("{task}: {}", got["error"].as_str().unwrap_or("no such task"))); }
        let asg = got["result"]["task"]["assignee"].as_str().unwrap_or("").to_string();
        if asg == receiver { return (true, String::new()); }
        if !asg.is_empty() && asg != sender { return (false, format!("{task} is {}'s task, so it stays theirs: ask them to re-assign it.", hub.name_of(&asg))); }
        let up = hub.board_tool("update", receiver, &json!({ "id": task, "assignee": receiver, "appendNote": format!("Handed off by {}'s agent; accepted by {}.", hub.name_of(sender), hub.name_of(receiver)) }));
        if up["ok"] == json!(true) { (true, String::new()) } else { (false, format!("{task}: {}", up["error"].as_str().unwrap_or("could not re-assign it"))) }
    }
    /// POST /api/agents/stop {thread} and /api/agents/reopen {thread} (either person; reopening resets the hop count).
    pub fn stop_reopen(&self, hub: &dyn AiHub, a: &Account, b: &Value, stop: bool) -> (u16, Value) {
        let tid = s(b, "thread");
        let Some(t) = load_thread(a, &tid) else { return (404, json!({ "error": "No such thread of yours." })) };
        let with = s(&t, "with");
        let me = a.username.clone();
        let out = self.with_thread(hub, &me, &with, &tid, None, |msgs, sh| {
            if stop { sh.insert("status".into(), json!("stopped")); sh.insert("why".into(), json!("stopped-by-person")); }
            else { sh.insert("status".into(), json!("open")); sh.insert("why".into(), json!("")); sh.insert("hopReset".into(), json!(msgs.iter().filter(|x| accepted(x)).filter_map(|x| x["hop"].as_u64()).max().unwrap_or(0))); }
            sh.insert("lastAt".into(), json!(now_ms()));
            msgs.last().cloned().unwrap_or(json!({ "thread": tid }))
        });
        let Some(last) = out else { return (404, json!({ "error": "No such thread of yours." })) };
        let mut line = last; line["thread"] = json!(tid);
        self.audit(hub, if stop { "stopped" } else { "reopened" }, &line, "", if stop { "stopped-by-person" } else { "" });
        if !stop { self.kick(); }
        (200, json!({ "ok": true, "thread": tid, "status": if stop { "stopped" } else { "open" } }))
    }
    /// POST /api/agents/settings {muted?, paused?, askFirst?, wake?}.
    pub fn set_settings(&self, hub: &dyn AiHub, a: &Account, b: &Value) -> (u16, Value) {
        let mut cur = settings_of(a);
        if let Some(l) = b["muted"].as_array() {
            let mut out: Vec<String> = vec![];
            for u in l.iter().filter_map(|x| x.as_str()) { let u = u.trim().trim_start_matches('@').to_lowercase(); if u == a.username { continue; } if hub.account(&u).is_none() { return (400, json!({ "error": format!("No account called \"{u}\".") })); } if !out.contains(&u) { out.push(u); } }
            cur["muted"] = json!(out);
        }
        if let Some(p) = b["paused"].as_bool() { cur["paused"] = json!(p); }
        if let Some(l) = b["askFirst"].as_array() {
            let ks: Vec<String> = l.iter().filter_map(|x| x.as_str()).map(String::from).collect();
            if let Some(k) = ks.iter().find(|k| !HOLDABLE.contains(&k.as_str())) { return (400, json!({ "error": format!("\"ask me first\" takes handoff, ask and review, not \"{}\".", k.chars().take(20).collect::<String>()) })); }
            cur["askFirst"] = json!(ks);
        }
        if let Some(w) = b["wake"].as_bool() { cur["wake"] = json!(w); }
        let _ = std::fs::create_dir_all(agents_dir(a));
        write_json(&settings_file(a), &cur);
        self.kick();
        (200, cur)
    }

    // ----- what people see -----
    fn summary(t: &Value) -> Value {
        let msgs = t["messages"].as_array().cloned().unwrap_or_default();
        let with = s(t, "with"); let seen = t["seenAt"].as_u64().unwrap_or(0);
        let unread = msgs.iter().filter(|m| s(&m["from"], "user") == with && m["at"].as_u64().unwrap_or(0) > seen).count();
        let last = msgs.last().cloned().unwrap_or(Value::Null);
        json!({ "id": t["id"], "with": with, "subject": t["subject"], "session": t["session"], "lastAt": t["lastAt"], "status": t["status"], "why": t["why"], "lastWhy": last["why"], "lastStatus": last["status"], "unread": unread, "count": msgs.len() })
    }
    /// The team's agents as `me` sees them (agents_list, GET /api/agents/list).
    pub fn list(&self, hub: &dyn AiHub, me: &Account) -> Value {
        let people = hub.team_tool("team_presence", &json!({}), me);
        let pres: HashMap<String, Value> = people["result"]["people"].as_array().into_iter().flatten().map(|p| (s(p, "user"), p.clone())).collect();
        let now = now_ms();
        let mut out = vec![];
        for a in hub.accounts() {
            if a.username == me.username { continue; }
            let set = settings_of(&a);
            let muted_you = set["muted"].as_array().is_some_and(|l| l.iter().any(|u| u == &json!(me.username)));
            let ix = sessions::index_if_any(&a).unwrap_or(Value::Null);
            let ext = sessions::ext_live(&a.username, now);
            let (state, source, session) = match resolve(&ix, &Address { user: a.username.clone(), ..Default::default() }, "", &ext) {
                Some(Target::Hub(sid)) => {
                    let key = if sid == s(&ix, "primary") { a.username.clone() } else { format!("{}#{sid}", a.username) };
                    let st = if ix["sessions"][&sid]["waiting"].is_object() { "waiting" } else if hub.work().busy_key(&key) { "working" } else { "idle" };
                    (st.to_string(), "work".to_string(), sid)
                }
                Some(Target::External { source, id, .. }) => { let st = ext.iter().find(|x| s(x, "id") == id).map(|x| s(x, "state")).unwrap_or_default(); (if st == "working" || st == "waiting" { st } else { "idle".into() }, source, id) }
                _ => ("none".to_string(), "work".to_string(), String::new()),
            };
            let p = pres.get(&a.username).cloned().unwrap_or(Value::Null);
            out.push(json!({ "user": a.username, "name": hub.name_of(&a.username), "address": format!("@{}", a.username), "reachable": !muted_you, "state": state, "source": source, "session": session,
                "acceptingWork": p["acceptingWork"] == json!(true), "paused": set["paused"], "mutedYou": muted_you, "askFirst": set["askFirst"], "presence": if p["presence"].is_string() { p["presence"].clone() } else { json!("offline") } }));
        }
        json!({ "agents": out })
    }

    // ----- delivery (section 4) -----
    fn load_pending(&self, hub: &dyn AiHub) {
        if lk(&self.pending).is_some() { return; }
        let mut l: Vec<Pend> = vec![];
        for a in hub.accounts() {
            for t in all_threads(&a) {
                for m in t["messages"].as_array().into_iter().flatten() {
                    if s(&m["to"], "user") != a.username { continue; }
                    let st = s(m, "status");
                    if st == "queued" || st == "held" || ((st == "delivered" || st == "read") && m["replyBy"].is_u64() && matches!(m["kind"].as_str(), Some("ask") | Some("review"))) { l.push(Pend { user: a.username.clone(), thread: s(m, "thread"), id: s(m, "id") }); }
                }
            }
        }
        let mut g = lk(&self.pending);
        let cur = g.get_or_insert_with(Vec::new);
        for p in l { if !cur.contains(&p) { cur.push(p); } }
    }
    /// An undelivered message (or a delivered ask nobody answered) past its reply-by: expired, the sender told once.
    fn expire(&self, hub: &dyn AiHub, m: &Value) {
        let Some(m2) = self.set_msg(hub, m, |x| { x["status"] = json!("expired"); x["why"] = json!("expired"); }) else { return };
        self.audit(hub, "expired", &m2, "", "expired");
        let id = s(m, "id");
        if let Some(w) = lk(&self.waits).get_mut(&id) { if w.result.is_none() { w.result = Some(json!({ "id": id, "thread": m["thread"], "expired": true, "text": format!("no answer from @{}'s agent by {}", s(&m["to"], "user"), hhmm(m["replyBy"].as_u64().unwrap_or(0))) })); w.notify.notify_waiters(); } }
        if matches!(m["kind"].as_str(), Some("ask") | Some("review") | Some("handoff")) { Self::tell(hub, &s(&m["from"], "user"), &format!("No answer from @{}'s agent by {} ({}: \"{}\").", s(&m["to"], "user"), hhmm(m["replyBy"].as_u64().unwrap_or(0)), s(m, "kind"), subject_of(&s(m, "text")))); }
    }
    /// One round of the pump: what can be delivered now goes, per session in one run; what is past its reply-by expires.
    async fn tick(hub: &Arc<dyn AiHub>) {
        let w = hub.work(); let ag = &w.agents;
        ag.load_pending(&**hub);
        let list = lk(&ag.pending).clone().unwrap_or_default();
        let lim = limits(&**hub); let now = now_ms();
        let mut drop: Vec<Pend> = vec![];
        let mut groups: Vec<(Account, Sx, Vec<Value>)> = vec![];
        let mut runs_of: HashMap<String, usize> = HashMap::new();
        for (_, u) in lk(&ag.running).iter() { *runs_of.entry(u.clone()).or_default() += 1; }
        for p in list {
            let Some(ra) = hub.account(&p.user) else { drop.push(p); continue };
            let Some(t) = load_thread(&ra, &p.thread) else { drop.push(p); continue };
            let Some(m) = t["messages"].as_array().and_then(|l| l.iter().find(|m| m["id"] == json!(p.id)).cloned()) else { drop.push(p); continue };
            let st = s(&m, "status"); let past = m["replyBy"].as_u64().is_some_and(|r| now > r);
            match st.as_str() {
                "queued" | "held" => { if past { ag.expire(&**hub, &m); drop.push(p); continue; } }
                "delivered" | "read" => { if past { ag.expire(&**hub, &m); drop.push(p); } continue; }
                _ => { drop.push(p); continue; }
            }
            if st == "held" { continue; }
            if s(&m["to"], "source") != "work" { continue; } // an external tool's inbox (T-299)
            let set = settings_of(&ra);
            let answer_to_own = matches!(m["kind"].as_str(), Some("reply") | Some("report")) && !s(&m, "parent").is_empty();
            if set["paused"] == json!(true) && !answer_to_own { continue; }
            let addr = Address { user: p.user.clone(), session: s(&m["x"], "session"), source: String::new() };
            let ix = sessions::index(&ra);
            let target = resolve(&ix, &addr, &s(&t, "session"), &sessions::ext_live(&p.user, now));
            let sx = match target {
                Some(Target::Hub(sid)) => sessions::resolve(&ra, &sid),
                Some(Target::Primary) => sessions::resolve(&ra, ""),
                Some(Target::External { source, device, id }) => {
                    // their agent is another tool now (section 2 rule 3): the message waits in that tool's inbox (T-299)
                    if ag.set_msg(&**hub, &m, |x| { x["to"]["source"] = json!(source); x["to"]["session"] = json!(id); if !device.is_empty() { x["x"]["device"] = json!(device); } }).is_some() { ag.inbox.notify_waiters(); }
                    continue;
                }
                None => None,
            };
            let Some(sx) = sx else {
                if let Some(m2) = ag.set_msg(&**hub, &m, |x| { x["status"] = json!("refused"); x["why"] = json!("no-session"); }) { ag.audit(&**hub, "refused", &m2, "", "no-session"); }
                Self::tell(&**hub, &s(&m["from"], "user"), &format!("Your agent's message to @{} could not be delivered: the session it named is gone.", p.user));
                drop.push(p); continue;
            };
            if w.busy_key(&sx.key) || lk(&ag.running).contains_key(&sx.key) { continue; }
            if lk(&ag.retry).get(&sx.key).is_some_and(|t| *t > now) { continue; }
            if set["wake"] == json!(false) && lk(&ag.ended).get(&sx.key).copied().unwrap_or(0) < m["at"].as_u64().unwrap_or(0) { continue; }
            if let Some(g) = groups.iter_mut().find(|g| g.1.key == sx.key) { g.2.push(m); continue; }
            if runs_of.get(&p.user).copied().unwrap_or(0) >= lim.max_agent_runs { continue; }
            *runs_of.entry(p.user.clone()).or_default() += 1;
            groups.push((ra, sx, vec![m]));
        }
        if !drop.is_empty() { if let Some(l) = lk(&ag.pending).as_mut() { l.retain(|x| !drop.contains(x)); } }
        for (ra, sx, msgs) in groups {
            lk(&ag.running).insert(sx.key.clone(), ra.username.clone());
            let h = hub.clone();
            tokio::spawn(async move { Self::deliver(h, ra, sx, msgs).await; });
        }
    }
    /// Delivers messages into one hub session: one run with them (queued behind its last reply, or waking it), as its person,
    /// with the session's own provider, model, project and worktree (POST /api/chat's own code), the transcript kept.
    async fn deliver(hub: Arc<dyn AiHub>, ra: Account, sx: Sx, mut msgs: Vec<Value>) {
        let w = hub.work(); let ag = &w.agents;
        msgs.sort_by_key(|m| m["at"].as_u64().unwrap_or(0));
        let text = msgs.iter().map(delivered_text).collect::<Vec<_>>().join("\n\n");
        let req = Req { method: hyper::Method::POST, path: "/api/chat".into(), query: String::new(), headers: hyper::HeaderMap::new(), body: Bytes::from(json!({ "message": text, "session": sx.sid }).to_string()), peer: std::net::SocketAddr::from(([127, 0, 0, 1], 0)) };
        let resp = crate::work::chat(&hub, &ra, &req).await;
        let done = |ag: &Agents| { lk(&ag.running).remove(&sx.key); ag.pump.notify_one(); };
        if resp.status() != 200 { lk(&ag.retry).insert(sx.key.clone(), now_ms() + 5000); done(ag); return; } // busy, its limits, a restart: later
        use http_body_util::BodyExt;
        let mut body = resp.into_body(); let mut buf = String::new();
        let (mut run, mut failed, mut texts) = (String::new(), String::new(), Vec::<String>::new());
        let mut delivered: Vec<Value> = vec![];
        // the run's final answer (its last assistant text: what a model without tools says) and what it used (§ 7, § 8)
        let (mut last_text, mut started) = (String::new(), 0u64);
        let (mut used, mut used_result) = (pgm_context_internal::ledger::Usage::default(), None::<pgm_context_internal::ledger::Usage>);
        let mut model_seen = String::new();
        while let Some(fr) = body.frame().await {
            let Ok(fr) = fr else { break };
            let Ok(data) = fr.into_data() else { continue };
            buf.push_str(&String::from_utf8_lossy(&data));
            while let Some(i) = buf.find('\n') {
                let line: String = buf.drain(..=i).collect();
                let Ok(ev) = serde_json::from_str::<Value>(line.trim()) else { continue };
                match ev["type"].as_str().unwrap_or("") {
                    "bridge_run" if run.is_empty() => {
                        run = s(&ev, "id"); started = now_ms();
                        // who started it (T-310): the sender, so the run reaches none of the receiver's machines or PC folders
                        if let Some(first) = msgs.first() { w.set_started_by(&run, &s(&first["from"], "user")); }
                        for m in &msgs {
                            let (sid, r2) = (sx.sid.clone(), run.clone());
                            if let Some(m2) = ag.set_msg(&*hub, m, |x| { if x["status"] == json!("queued") { x["status"] = json!("delivered"); } x["to"]["session"] = json!(sid); }) {
                                ag.audit(&*hub, "delivered", &m2, &r2, "");
                                w.emit_key(&sx.key, json!({ "type": "agent_message", "message": envelope(&m2) }));
                                delivered.push(m2);
                            }
                            let sid2 = sx.sid.clone();
                            ag.with_side(&ra, &s(m, "thread"), |t| { if s(t, "session").is_empty() { t["session"] = json!(sid2); } });
                        }
                    }
                    "bridge_error" if run.is_empty() => { failed = s(&ev, "text"); }
                    "assistant" => {
                        let mut this: Vec<String> = vec![];
                        for c in ev["message"]["content"].as_array().into_iter().flatten() { if c["type"] == json!("text") { let t = s(c, "text"); if !t.trim().is_empty() { this.push(t); } } }
                        if !this.is_empty() { last_text = this.join("\n\n"); texts.extend(this); }
                        if let Some(u) = ev["message"]["usage"].as_object().and_then(|_| crate::openai::normalise(&ev["message"]["usage"])) { used.add(&u); }
                        if let Some(md) = ev["message"]["model"].as_str().filter(|x| !x.is_empty()) { model_seen = md.to_string(); }
                    }
                    "result" => { if let Some(u) = ev["usage"].as_object().and_then(|_| crate::openai::normalise(&ev["usage"])) { used_result = Some(u); } }
                    _ => {}
                }
            }
        }
        if run.is_empty() {
            // the run never started (no AI for Work, no worktree): nobody can receive it now
            for m in &msgs {
                if let Some(m2) = ag.set_msg(&*hub, m, |x| { x["status"] = json!("refused"); x["why"] = json!("no-session"); }) { ag.audit(&*hub, "refused", &m2, "", "no-session"); }
                Self::tell(&*hub, &s(&m["from"], "user"), &format!("Your agent's message to @{} could not be delivered: {}", ra.username, if failed.is_empty() { "their Work did not start.".to_string() } else { failed.chars().take(300).collect() }));
            }
            if let Some(l) = lk(&ag.pending).as_mut() { l.retain(|p| !msgs.iter().any(|m| m["id"] == json!(p.id))); }
            done(ag); return;
        }
        // that run has ended: the agent saw them (read); an ask / review stays waited on until its reply-by
        for m in &delivered {
            let r2 = run.clone();
            if let Some(m2) = ag.set_msg(&*hub, m, |x| { if x["status"] == json!("delivered") { x["status"] = json!("read"); } }) { ag.audit(&*hub, "read", &m2, &r2, ""); }
            let keep = matches!(m["kind"].as_str(), Some("ask") | Some("review")) && m["replyBy"].is_u64();
            if !keep { if let Some(l) = lk(&ag.pending).as_mut() { l.retain(|p| p.id != s(m, "id")); } }
        }
        // the cost (section 8): an agent-started run is the receiver's run: their time log says which agent started it, and what
        // it used goes to their usage ledger (answers.by.<provider kind>), never the sender's
        let starter = msgs.first().map(|m| s(&m["from"], "user")).unwrap_or_default();
        let st = sessions::sstate(w, &ra, &sx);
        hub.note_agent_run(&ra.username, &starter, &run, now_ms().saturating_sub(started));
        let used = used_result.unwrap_or(used);
        if used.input + used.output + used.cache_read + used.cache_write > 0 {
            let kind = w.effective(hub.ai(), &ra.username, &s(&st, "provider")).map(|p| s(&p, "kind")).unwrap_or_default();
            let model = if model_seen.is_empty() { s(&st, "model") } else { model_seen.clone() };
            hub.ai().usage.note_answer(&ra.username, &kind, &model, &used);
        }
        // a model without tools (section 7): an ask / review whose sender waits, that this run did not answer with agent_reply,
        // gets the run's final answer as its reply, marked by "final-answer"
        if !last_text.trim().is_empty() {
            let model = { let md = s(&st, "model"); if md.is_empty() { w.default_model_of(&ra) } else { md } };
            for m in &delivered {
                if m["x"]["wait"] != json!(true) || !matches!(m["kind"].as_str(), Some("ask") | Some("review")) { continue; }
                let now_st = load_thread(&ra, &s(m, "thread")).and_then(|t| t["messages"].as_array().and_then(|l| l.iter().find(|x| x["id"] == m["id"]).map(|x| s(x, "status")))).unwrap_or_default();
                if !matches!(now_st.as_str(), "delivered" | "read") { continue; } // answered, expired, refused: nothing to add
                let from = json!({ "user": ra.username, "session": sx.sid, "source": "work", "model": model, "run": run });
                let (code, v) = ag.send_by(&*hub, &from, &json!({ "thread": m["thread"], "kind": "reply", "text": last_text, "parent": m["id"] }), true, None, "final-answer");
                if code != 202 { eprintln!("[agents] the final answer of run {run} could not be sent as the reply: {}", v["error"].as_str().unwrap_or("")); }
            }
        }
        // the session's conversation: the message as it arrived, then the answer (what the page shows after a reload)
        let mut cur = sessions::scurrent(&ra, &sx);
        if let Some(tr) = cur["transcript"].as_array_mut() {
            tr.push(json!({ "cls": "me", "text": text }));
            let answer: String = texts.join("\n\n").chars().take(200 * 1024).collect();
            if !answer.is_empty() { tr.push(json!({ "cls": "claude", "text": answer })); }
        }
        cur["run"] = Value::Null;
        sessions::save_scurrent(&ra, &sx, &mut cur);
        done(ag);
    }
}

/// The delivery pump (started with the Work runs): every few seconds, and at once on a send, a setting or a run that ended.
pub fn start(hub: &Arc<dyn AiHub>) {
    let h = hub.clone();
    tokio::spawn(async move { loop {
        let ag = &h.work().agents;
        let n = ag.pump.notified(); tokio::pin!(n); n.as_mut().enable();
        if let Err(e) = tokio::spawn({ let h = h.clone(); async move { Agents::tick(&h).await } }).await { eprintln!("[agents] the delivery round failed: {e}"); }
        let _ = tokio::time::timeout(Duration::from_secs(3), n).await;
    } });
}

// ---------- the routes (section 6; the JavaScript hub answers 404) ----------
fn person_from(a: &Account) -> Value { json!({ "user": a.username, "session": "", "source": "person", "model": "", "run": "" }) }
/// The sender of a send / reply over HTTP: the person on the page, or (T-299) the person's other tool when the body names a
/// `source` (jcode, hpm …) that tool reports sessions for (T-289's report: that is where the message came in, so the field stays
/// honest); `session` is one of the sessions it reports, `model` what that tool says it runs (a name, never trusted for more).
fn http_from(a: &Account, b: &Value) -> Result<Value, (u16, Value)> {
    let src = s(b, "source").trim().to_lowercase();
    if src.is_empty() || src == "person" { return Ok(person_from(a)); }
    if src == "work" { return Err((400, json!({ "error": "Work's runs send with the agent tools; from here the source is another tool's (jcode, hpm …).", "why": "bad" }))); }
    let ext = sessions::ext_live(&a.username, now_ms());
    let mine: Vec<&Value> = ext.iter().filter(|x| s(x, "source") == src).collect();
    if mine.is_empty() { return Err((400, json!({ "error": format!("No live {src} sessions of yours: report them first (POST /api/work/sessions/report)."), "why": "bad" }))); }
    let want = s(b, "session");
    let session = if want.is_empty() { mine.iter().max_by_key(|x| x["updatedAt"].as_u64().unwrap_or(0)).map(|x| s(x, "id")).unwrap_or_default() }
        else if mine.iter().any(|x| s(x, "id") == want) { want } else { return Err((400, json!({ "error": format!("\"{}\" is not one of the {src} sessions you report.", want.chars().take(40).collect::<String>()), "why": "bad" }))); };
    let model: String = s(b, "model").chars().filter(|c| c.is_alphanumeric() || "-_.:/ ".contains(*c)).take(60).collect();
    Ok(json!({ "user": a.username, "session": session, "source": src, "model": model, "run": "" }))
}
/// GET /api/agents/inbox (T-299, section 7): the messages waiting for the person's other tool `source` (on `device`, or any
/// device when the message names none). Fetched = delivered (the tool shows them in the session `to.session` names); `again`
/// also returns the ones it fetched before and has not marked read (after the tool restarted). Same envelope as everywhere.
fn inbox_take(hub: &dyn AiHub, ag: &Agents, a: &Account, source: &str, device: &str, again: bool) -> Vec<Value> {
    let paused = settings_of(a)["paused"] == json!(true);
    let mut out: Vec<Value> = vec![];
    let mut found: Vec<Value> = all_threads(a).into_iter().flat_map(|t| t["messages"].as_array().cloned().unwrap_or_default()).filter(|m| {
        let st = s(m, "status");
        s(&m["to"], "user") == a.username && s(&m["to"], "source") == source && (st == "queued" || (again && st == "delivered"))
            && { let d = s(&m["x"], "device"); d.is_empty() || device.is_empty() || d == device }
            && (!paused || (matches!(m["kind"].as_str(), Some("reply") | Some("report")) && !s(m, "parent").is_empty()))
    }).collect();
    found.sort_by_key(|m| m["at"].as_u64().unwrap_or(0));
    let ext = sessions::ext_live(&a.username, now_ms());
    for m in found {
        // the session it goes to: the one it names, else the tool's newest on that device
        let sid = { let x = s(&m["to"], "session"); if !x.is_empty() { x } else { ext.iter().filter(|e| s(e, "source") == source && (device.is_empty() || s(e, "device") == device)).max_by_key(|e| e["updatedAt"].as_u64().unwrap_or(0)).map(|e| s(e, "id")).unwrap_or_default() } };
        let dev = device.to_string();
        let Some(m2) = ag.set_msg(hub, &m, |x| { if x["status"] == json!("queued") { x["status"] = json!("delivered"); } x["to"]["session"] = json!(sid); if !dev.is_empty() { x["x"]["device"] = json!(dev); } }) else { continue };
        if m["status"] == json!("queued") { ag.audit(hub, "delivered", &m2, "", ""); }
        let sid2 = sid.clone(); let src = source.to_string();
        if !sid2.is_empty() { ag.with_side(a, &s(&m, "thread"), |t| { if s(t, "session").is_empty() { t["session"] = json!(sid2); t["source"] = json!(src); } }); }
        out.push(envelope(&m2));
    }
    out
}
fn wants_wait(b: &Value) -> bool { b["wait"] == json!(true) && matches!(b["kind"].as_str(), Some("ask") | Some("review") | None) }

/// /api/agents/* signed in as `a`; None = not one of them.
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    let p = r.path.strip_prefix("/api/agents/")?;
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let ag = &hub.work().agents;
    let num = |k: &str| r.param(k).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    Some(match p {
        "send" if post => {
            let b = r.json();
            let from = match http_from(a, &b) { Ok(f) => f, Err((st, v)) => return Some(answer(st, v)) };
            let wait = wants_wait(&b);
            let (st, v) = ag.send(&**hub, &from, &b, false, if wait { Some((String::new(), String::new())) } else { None });
            if st != 202 || !wait || v["status"] == json!("held") { return Some(answer(st, v)); }
            let out = ag.wait_for(&**hub, &a.username, &s(&v, "id"), POLL_SEC).await;
            if out["reply"].is_object() { answer(200, json!({ "id": v["id"], "thread": v["thread"], "reply": out["reply"] })) } else { let mut o = v; o["pending"] = json!(true); answer(202, o) }
        }
        "wait" if get => {
            let id = r.param("id").unwrap_or_default();
            let poll = if num("pollSec") == 0 { POLL_SEC } else { num("pollSec").clamp(1, POLL_SEC) };
            let out = ag.wait_for(&**hub, &a.username, &id, poll).await;
            if out["reply"].is_object() { answer(200, out) } else { answer(202, out) }
        }
        "reply" if post => {
            let b = r.json();
            let from = match http_from(a, &b) { Ok(f) => f, Err((st, v)) => return Some(answer(st, v)) };
            let (st, v) = ag.send(&**hub, &from, &b, true, None); answer(st, v)
        }
        "inbox" if get => {
            let source = r.param("source").unwrap_or_default().trim().to_lowercase();
            if source.is_empty() || source == "work" || source.len() > 24 || !source.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') { return Some(answer(400, json!({ "error": "The inbox is another tool's: name its source (e.g. source=jcode)." }))); }
            let device = r.param("device").unwrap_or_default();
            let again = r.param("again").is_some_and(|x| x == "1" || x == "true");
            let wait = r.param("wait").is_some_and(|x| x == "1" || x == "true");
            let secs = if num("waitSec") == 0 { INBOX_WAIT_SEC } else { num("waitSec").clamp(1, INBOX_WAIT_SEC) };
            let end = std::time::Instant::now() + Duration::from_secs(secs);
            loop {
                let n = ag.inbox.notified(); tokio::pin!(n); n.as_mut().enable();
                let got = inbox_take(&**hub, ag, a, &source, &device, again);
                let left = end.saturating_duration_since(std::time::Instant::now());
                if !got.is_empty() || !wait || left.is_zero() { break answer(200, json!({ "messages": got })); }
                let _ = tokio::time::timeout(left.min(Duration::from_secs(5)), n).await;
            }
        }
        "read" if post => {
            let b = r.json();
            let ids: Vec<String> = b["ids"].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).take(200).collect()).unwrap_or_default();
            if ids.is_empty() { return Some(answer(400, json!({ "error": "Name the messages: {ids: [\"am-…\"]}." }))); }
            let (mut read, mut unknown) = (vec![], vec![]);
            for id in ids {
                let Some(m) = Agents::find_msg(a, &id).filter(|m| s(&m["to"], "user") == a.username) else { unknown.push(id); continue };
                if !matches!(m["status"].as_str(), Some("delivered") | Some("queued")) { continue; } // read already, answered …: as it is
                if let Some(m2) = ag.set_msg(&**hub, &m, |x| { x["status"] = json!("read"); }) { ag.audit(&**hub, "read", &m2, "", ""); }
                // an ask / review stays watched until its reply-by (it expires on the sender's side); the rest is done
                if !(matches!(m["kind"].as_str(), Some("ask") | Some("review")) && m["replyBy"].is_u64()) { if let Some(l) = lk(&ag.pending).as_mut() { l.retain(|p| p.id != id); } }
                read.push(id);
            }
            if read.is_empty() && !unknown.is_empty() { answer(404, json!({ "error": "No such message for you.", "unknown": unknown })) } else { answer(200, json!({ "ok": true, "read": read, "unknown": unknown })) }
        }
        "threads" if get => {
            let with = r.param("with").unwrap_or_default(); let since = num("since");
            let mut l: Vec<Value> = all_threads(a).iter().filter(|t| (with.is_empty() || s(t, "with") == with) && t["lastAt"].as_u64().unwrap_or(0) >= since).map(Agents::summary).collect();
            l.sort_by_key(|t| std::cmp::Reverse(t["lastAt"].as_u64().unwrap_or(0)));
            answer(200, json!({ "threads": l }))
        }
        "thread" if get => {
            let id = r.param("id").unwrap_or_default();
            let Some(t) = load_thread(a, &id) else { return Some(answer(404, json!({ "error": "No such thread of yours." }))) };
            ag.with_side(a, &id, |x| { x["seenAt"] = json!(now_ms()); });
            let mut meta = t.clone(); if let Some(o) = meta.as_object_mut() { o.remove("messages"); }
            let mut out = t; out["thread"] = meta;
            answer(200, out)
        }
        "held" if post => { let (st, v) = ag.held(&**hub, a, &r.json()); answer(st, v) }
        "stop" if post => { let (st, v) = ag.stop_reopen(&**hub, a, &r.json(), true); answer(st, v) }
        "reopen" if post => { let (st, v) = ag.stop_reopen(&**hub, a, &r.json(), false); answer(st, v) }
        "settings" if get => answer(200, settings_of(a)),
        "settings" if post => { let (st, v) = ag.set_settings(&**hub, a, &r.json()); answer(st, v) }
        "list" if get => answer(200, ag.list(&**hub, a)),
        "audit" if get => {
            let since = num("since"); let user = r.param("user").unwrap_or_default();
            let names = |l: &Value, u: &str| s(&l["from"], "user") == u || s(&l["to"], "user") == u;
            let lines: Vec<Value> = Agents::audit_lines(&**hub, since).into_iter().filter(|l| if a.admin { user.is_empty() || names(l, &user) } else { names(l, &a.username) && (user.is_empty() || names(l, &user)) }).collect();
            answer(200, json!({ "lines": lines }))
        }
        _ => answer(404, json!({ "error": "Unknown endpoint" })),
    })
}

// ---------- the team tools (T-297; bridge/team-mcp.js and host/mcp-tools.json) ----------
/// The envelope's `from` of a run: its person, session, model and run id (as the hub's records say, never the body).
fn run_from(hub: &dyn AiHub, caller: &Caller, a: &Account) -> (Value, Option<Sx>) {
    let w = hub.work();
    let sx = sessions::resolve(a, &caller.sid);
    let model = sx.as_ref().map(|sx| s(&sessions::sstate(w, a, sx), "model")).filter(|m| !m.is_empty()).unwrap_or_else(|| w.default_model_of(a));
    (json!({ "user": a.username, "session": sx.as_ref().map(|x| x.sid.clone()).unwrap_or_default(), "source": "work", "model": model, "run": caller.run }), sx)
}
fn refused(v: &Value) -> Value { json!({ "ok": false, "error": v["error"].as_str().unwrap_or("The hub refused the message.") }) }
/// agents_list, agent_ask, agent_handoff, agent_review, agent_report, agent_reply; None = not one of them.
pub async fn tool(hub: &Arc<dyn AiHub>, caller: &Caller, a: &Account, name: &str, args: &Value) -> Option<Value> {
    if !(name == "agents_list" || name.starts_with("agent_")) { return None; }
    let ag = &hub.work().agents;
    let ok = |v: Value| json!({ "ok": true, "result": v });
    if name == "agents_list" { return Some(ok(ag.list(&**hub, a))); }
    let (from, sx) = run_from(&**hub, caller, a);
    let poll = args["pollSec"].as_u64().unwrap_or(POLL_SEC).clamp(1, POLL_SEC);
    let sent = |v: &Value| { if !caller.run.is_empty() { hub.work().emit_run(&caller.run, json!({ "type": "agent_sent", "id": v["id"], "thread": v["thread"], "to": v["to"] })); } };
    Some(match name {
        "agent_ask" | "agent_review" => {
            if let Some(id) = args["poll"].as_str() { let out = ag.wait_for(&**hub, &a.username, id, poll).await; return Some(ok(out)); }
            let kind = if name == "agent_ask" { "ask" } else { "review" };
            if kind == "review" && clean_refs(&args["refs"]).as_object().map(|o| o.is_empty()).unwrap_or(true) { return Some(json!({ "ok": false, "error": "agent_review names what to look at: refs.commit, refs.branch or refs.note." })); }
            let wait = args["wait"] != json!(false);
            let mut b = json!({ "to": args["to"], "kind": kind, "text": args["text"], "refs": args["refs"], "thread": args["thread"] });
            if let Some(x) = args["replyBySec"].as_u64() { b["replyBySec"] = json!(x); }
            let wk = if wait { sx.as_ref().map(|sx| (sx.key.clone(), sx.sid.clone())).or(Some((String::new(), String::new()))) } else { None };
            let (st, v) = ag.send(&**hub, &from, &b, false, wk);
            if st != 202 { return Some(refused(&v)); }
            sent(&v);
            if !wait || v["status"] == json!("held") { let mut o = v.clone(); o["sent"] = json!(true); o["pending"] = json!(true); return Some(ok(o)); }
            let out = ag.wait_for(&**hub, &a.username, &s(&v, "id"), poll).await;
            ok(if out["pending"] == json!(true) && out["sent"] != json!(true) { json!({ "pending": true, "id": v["id"], "thread": v["thread"] }) } else { out })
        }
        "agent_handoff" => {
            let mut refs = if args["refs"].is_object() { args["refs"].clone() } else { json!({}) };
            if let Some(t) = args["task"].as_str() { refs["task"] = json!(t); }
            let (st, v) = ag.send(&**hub, &from, &json!({ "to": args["to"], "kind": "handoff", "text": args["text"], "refs": refs, "thread": args["thread"] }), false, None);
            if st != 202 { return Some(refused(&v)); }
            sent(&v);
            ok(json!({ "id": v["id"], "thread": v["thread"], "status": v["status"], "note": v["note"] }))
        }
        "agent_report" | "agent_reply" => {
            let mut text = s(args, "text");
            if name == "agent_report" {
                let st = s(args, "status");
                if !matches!(st.as_str(), "done" | "blocked" | "need") { return Some(json!({ "ok": false, "error": "agent_report's status is done, blocked or need." })); }
                text = format!("[{st}] {text}");
            }
            let (st, v) = ag.send(&**hub, &from, &json!({ "thread": args["thread"], "kind": if name == "agent_report" { "report" } else { "reply" }, "text": text, "refs": args["refs"] }), true, None);
            if st != 202 { return Some(refused(&v)); }
            sent(&v);
            ok(json!({ "id": v["id"], "thread": v["thread"], "status": v["status"] }))
        }
        _ => json!({ "ok": false, "error": format!("Unknown agent tool: {name}") }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> Value {
        json!({ "primary": "s-000000000001", "current": "s-000000000002", "order": ["s-000000000002", "s-000000000001", "s-000000000003"], "sessions": {
            "s-000000000001": { "title": "first" }, "s-000000000002": { "title": "now" }, "s-000000000003": { "title": "old", "archived": true } } })
    }
    fn ext(id: &str, state: &str, at: u64) -> Value { json!({ "source": "jcode", "device": "box", "id": id, "state": state, "updatedAt": at }) }
    fn to(u: &str) -> Address { parse_address(u).unwrap() }

    #[test]
    fn an_address_names_a_person_a_session_or_a_tool() {
        assert_eq!(to("@gee"), Address { user: "gee".into(), session: "".into(), source: "".into() });
        assert_eq!(to("Gee").user, "gee");
        assert_eq!(to("@gee#s-1a2b3c4d5e6f").session, "s-1a2b3c4d5e6f");
        assert_eq!(to("@gee/jcode").source, "jcode");
        assert_eq!(to("@gee/work").source, "", "Work is the hub's own: no source");
        for bad in ["", "@", "@g e", "@gee#s-12", "@gee#x-1a2b3c4d5e6f", "@gee/j code", "@../x"] { assert!(parse_address(bad).is_none(), "{bad}"); }
    }
    #[test]
    fn the_resolver_follows_section_2() {
        let ix = index();
        // 1. a named session wins (even when it is not current); an archived or unknown one does not resolve
        assert_eq!(resolve(&ix, &to("@gee#s-000000000001"), "", &[]), Some(Target::Hub("s-000000000001".into())));
        assert_eq!(resolve(&ix, &to("@gee#s-000000000003"), "", &[]), None);
        assert_eq!(resolve(&ix, &to("@gee#s-0000000000ff"), "", &[]), None);
        // the session a thread sticks to beats the current one, while it exists and is not archived
        assert_eq!(resolve(&ix, &to("@gee"), "s-000000000001", &[]), Some(Target::Hub("s-000000000001".into())));
        assert_eq!(resolve(&ix, &to("@gee"), "s-000000000003", &[]), Some(Target::Hub("s-000000000002".into())));
        assert_eq!(resolve(&ix, &to("@gee"), "s-0000000000ff", &[]), Some(Target::Hub("s-000000000002".into())));
        // 2. the current one
        assert_eq!(resolve(&ix, &to("@gee"), "", &[ext("j1", "working", 9)]), Some(Target::Hub("s-000000000002".into())), "hub sessions win over external ones");
        // an archived or hidden current is none: 4. the primary
        let mut h = ix.clone(); h["sessions"]["s-000000000002"]["hidden"] = json!(true);
        assert_eq!(resolve(&h, &to("@gee"), "", &[]), Some(Target::Hub("s-000000000001".into())));
        // the primary hidden too: the first usable one in order, else the primary all the same
        h["sessions"]["s-000000000001"]["hidden"] = json!(true);
        assert_eq!(resolve(&h, &to("@gee"), "", &[]), Some(Target::Hub("s-000000000001".into())));
        // 3. no hub sessions at all: the newest working / waiting external one, else the newest; none at all: make the primary
        let none = json!({});
        assert_eq!(resolve(&none, &to("@gee"), "", &[ext("j1", "idle", 30), ext("j2", "working", 10), ext("j3", "waiting", 20)]), Some(Target::External { source: "jcode".into(), device: "box".into(), id: "j3".into() }));
        assert_eq!(resolve(&none, &to("@gee"), "", &[ext("j1", "idle", 30), ext("j2", "ready", 40)]), Some(Target::External { source: "jcode".into(), device: "box".into(), id: "j2".into() }));
        assert_eq!(resolve(&Value::Null, &to("@gee"), "", &[]), Some(Target::Primary));
        // a named tool: its sessions only
        assert_eq!(resolve(&ix, &to("@gee/jcode"), "", &[ext("j1", "idle", 30)]), Some(Target::External { source: "jcode".into(), device: "box".into(), id: "j1".into() }));
        assert_eq!(resolve(&ix, &to("@gee/hpm"), "", &[ext("j1", "idle", 30)]), None);
    }
    #[test]
    fn the_loop_detector_needs_alternation_sameness_and_no_new_refs() {
        let m = |who: &str, t: &str| (who.to_string(), t.to_string(), json!({}));
        let echo = vec![m("a", "ECHO r484 ping"), m("b", "ECHO r484 ping"), m("a", "ECHO r484 ping"), m("b", "ECHO r484 ping")];
        assert!(!is_loop(&echo, 3, 0.9), "four messages: one exchange short of the window");
        let mut five = echo.clone(); five.push(m("a", "echo, R484: PING!"));
        assert!(is_loop(&five, 3, 0.9), "normalised: case and punctuation do not count");
        // a text that changes enough is progress
        let mut moved = echo.clone(); moved.push(m("a", "ECHO r484 ping but now with the actual answer"));
        assert!(!is_loop(&moved, 3, 0.9));
        // a new ref is progress
        let mut refd = echo.clone(); refd.push(("a".into(), "ECHO r484 ping".into(), json!({ "commit": "abc123" })));
        assert!(!is_loop(&refd, 3, 0.9));
        // a ref the thread had already is not
        let mut old = vec![("a".to_string(), "ECHO r484 ping".to_string(), json!({ "task": "T-1" }))]; old.extend(echo.iter().skip(1).cloned()); old.push(("a".into(), "ECHO r484 ping".into(), json!({ "task": "T-1" })));
        assert!(is_loop(&old, 3, 0.9));
        // the same person twice in a row is not A→B→A
        let mut same = echo.clone(); same.push(m("b", "ECHO r484 ping"));
        assert!(!is_loop(&same, 3, 0.9));
        assert_eq!(jaccard("", ""), 1.0);
        assert!((jaccard("a b c d", "a b c e") - 0.6).abs() < 1e-9);
    }
    #[test]
    fn the_rate_limits_count_per_pair_and_per_sender_in_an_hour() {
        let l = Limits::default();
        let mut r = RateLog::default();
        let t0 = 1_000_000_000u64;
        for i in 0..20 { assert!(r.check(t0 + i, "a", "b", &l).is_ok(), "message {i}"); r.note(t0 + i, "a", "b"); }
        let e = r.check(t0 + 30, "a", "b", &l).unwrap_err();
        assert!(e.contains("20 messages an hour between you and @b"), "{e}");
        assert!(r.check(t0 + 30, "b", "a", &l).is_ok(), "the other way is its own count");
        assert!(r.check(t0 + 30, "a", "c", &l).is_ok(), "another person is its own pair");
        assert!(r.check(t0 + 3_600_001, "a", "b", &l).is_ok(), "an hour later the pair may again");
        let mut s2 = RateLog::default();
        for i in 0..60u64 { let to = format!("p{}", i % 4); s2.note(t0 + i, "a", &to); }
        assert!(s2.check(t0 + 100, "a", "p9", &l).unwrap_err().contains("60 agent messages an hour"));
        let c = Limits::from_config(&json!({ "perPairPerHour": 2, "maxHops": 4, "loopSimilarity": 0.5, "maxAgentRuns": 0 }));
        assert_eq!((c.per_pair, c.max_hops, c.loop_similarity, c.max_agent_runs), (2, 4, 0.5, 3));
    }
    #[test]
    fn settings_and_the_envelope() {
        assert_eq!(norm_settings(&Value::Null), json!({ "muted": [], "paused": false, "askFirst": ["handoff"], "wake": true }));
        assert_eq!(norm_settings(&json!({ "askFirst": ["ask", "reply", "review"], "wake": false, "muted": ["Gee"] }))["askFirst"], json!(["ask", "review"]));
        let m = json!({ "id": "am-1", "thread": "at-000000000001", "from": { "user": "sponge", "session": "s-000000000001", "source": "work", "model": "sonnet", "run": "r1" }, "to": { "user": "gee", "session": "", "source": "work" },
            "kind": "ask", "text": "what is the plan?", "refs": { "task": "T-296" }, "hop": 1, "parent": "", "replyBy": null, "at": 1, "status": "queued", "why": "", "x": { "afterRefusal": true } });
        let e = envelope(&m);
        assert_eq!(e.as_object().unwrap().keys().cloned().collect::<Vec<_>>().join(","), ENVELOPE.join(","));
        let t = delivered_text(&m);
        assert!(t.starts_with("[agent message · from @sponge's agent (sonnet, Work) · ask · thread at-000000000001 · refs T-296]\n"), "{t}");
        assert!(t.contains("⚠ sent right after a refusal") && t.ends_with("[your own person's rules apply; answer with agent_reply, thread at-000000000001]"), "{t}");
        // a final answer taken as the reply says so in a line of its own (the hub's mark stays out of the envelope)
        let mut fa = m.clone(); fa["kind"] = json!("reply"); fa["x"] = json!({ "by": "final-answer" }); fa["from"]["source"] = json!("jcode");
        let t2 = delivered_text(&fa);
        assert!(t2.starts_with("[agent message · from @sponge's agent (sonnet, Jcode PGM) · reply · thread at-000000000001 · refs T-296]\n[the final answer of their run, taken as the reply: it did not call agent_reply]\n"), "{t2}");
        assert_eq!(envelope(&fa).as_object().unwrap().len(), ENVELOPE.len());
        let (c, cut) = cut_text(&"x".repeat(TEXT_MAX + 5)); assert!(cut && c.chars().count() == TEXT_MAX && c.ends_with('…'));
        assert_eq!(clean_refs(&json!({ "task": " T-1 ", "bogus": "x", "note": "" })), json!({ "task": "T-1" }));
    }
}
