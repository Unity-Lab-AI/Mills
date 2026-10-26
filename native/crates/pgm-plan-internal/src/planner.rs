//! Planner helpers (T-276 intake, T-277 owners, T-278 vocabulary; team note plans/planner-mode.md § 2 to 5): the
//! deterministic parts of planning, so the planner AI does not guess them. Pure functions over text and board cards
//! (this file), and [`tool`], the hub side of five team MCP tools:
//! - `plan_parse {text}`: the input split into ideas (forms A `---` blocks, B person sections, C a short and a long
//!   version merged, D run-on chat), each idea with its title, body, model tag, person, blocked-on and source lines,
//!   plus what is not an idea;
//! - `plan_existing {title, words?}`: open and recently done board tasks that look like the same thing;
//! - `plan_roles`: the team's roles and each person's open workload (the roles file `plans/roles.md` in the team
//!   notes, else the Roles of `plans/task-assignments.md`, else a built-in list);
//! - `plan_owner {idea}`: a suggested owner with the reason (marker, then role keywords, then the lighter workload;
//!   never empty);
//! - `plan_glossary`: the words and decisions of `plans/glossary.md`.
//!
//! Nothing here writes anything; the planner shows its draft and the person approves it first (rule 7).
use crate::write::PlanHub;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::OnceLock;

macro_rules! re { ($p:expr) => {{ static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new($p).expect("regex")) }}; }

/// The team note the roles are read from (JSON, see [`roles_from_json`]).
pub const ROLES_NOTE: &str = "plans/roles.md";
/// The note whose "## Roles" section is the fallback.
pub const ASSIGNMENTS_NOTE: &str = "plans/task-assignments.md";
/// The team's words and decisions.
pub const GLOSSARY_NOTE: &str = "plans/glossary.md";
/// The roles file this module writes as an example and reads back (a proposal; Mills / Sponge keep the real one).
pub const ROLES_EXAMPLE: &str = include_str!("planner-roles.json");

// ---------- words ----------
const STOP: &[&str] = &["a", "an", "the", "and", "or", "of", "to", "in", "on", "for", "with", "is", "are", "be", "it", "this", "that", "we", "our", "us", "you", "your", "as", "at", "by", "from", "can", "should", "must", "will", "would", "could", "need", "needs", "want", "also", "so", "if", "then", "than", "too", "very", "just", "some", "any", "all", "one", "more", "such", "have", "has", "had", "do", "does", "not", "no", "but", "into", "how", "what", "when", "which", "who", "may", "might", "there", "their", "they", "them", "these", "those", "its", "i", "me", "my", "about", "over", "per", "via", "new", "able", "way", "etc", "ext"];

fn stem(w: &str) -> String {
    let mut s = w.to_string();
    if s.len() > 6 && s.ends_with("ing") { s.truncate(s.len() - 3); }
    else if s.len() > 5 && s.ends_with("ed") { s.truncate(s.len() - 2); }
    else if s.len() > 3 && s.ends_with('s') && !s.ends_with("ss") { s.pop(); }
    if s.len() > 3 && s.ends_with('e') { s.pop(); }
    s
}
fn raw_words(text: &str) -> Vec<String> { text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 2).map(String::from).collect() }
/// The content words of a text, stemmed (stop words, task ids and bare numbers left out), in order, with repeats.
pub fn words(text: &str) -> Vec<String> {
    raw_words(text).into_iter().filter(|w| !STOP.contains(&w.as_str()) && !re!(r"^t\d+$").is_match(w) && !w.chars().all(|c| c.is_ascii_digit())).map(|w| stem(&w)).collect()
}
fn wordset(text: &str) -> BTreeSet<String> { words(text).into_iter().collect() }
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 { if a.is_empty() || b.is_empty() { return 0.0; } a.intersection(b).count() as f64 / a.union(b).count() as f64 }
fn norm_title(t: &str) -> String { raw_words(t).join(" ") }
fn s_of(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_string() }

// ---------- roles (T-277) ----------
#[derive(Clone, Debug)]
pub struct Role { pub user: String, pub name: String, pub initial: String, pub specialties: Vec<String>, pub keywords: Vec<String> }
#[derive(Clone, Debug)]
pub struct Roles { pub people: Vec<Role>, pub fallback: String, pub source: String }

/// What each person is the natural owner of when the roles file gives no keywords (plans/planner-mode.md § 4.3).
fn default_keywords(user: &str) -> &'static [&'static str] {
    match user {
        "sponge" => &["hub", "protocol", "infrastructure", "sysadmin", "server", "spec", "test", "debug", "prompt", "release", "deploy", "updater", "login", "session", "git", "screen sharing", "sound", "download", "relay", "backend", "gateway", "rust"],
        "mills" => &["design", "ui", "ux", "panel", "sidebar", "menu", "right click", "layout", "visual", "decomposition", "refine", "document", "docs", "legal", "scope", "organisation", "archive", "calendar", "website", "builder", "drag", "theme", "graphic", "look"],
        "gee" => &["architecture", "research", "creative", "concept", "workflow", "node", "ollama", "model", "llm", "jev", "delegation", "orchestration", "investor", "trigger", "schedule", "integration"],
        _ => &[],
    }
}
fn builtin_person(user: &str, name: &str, spec: &[&str]) -> Role { Role { user: user.into(), name: name.into(), initial: name[..1].to_uppercase(), specialties: spec.iter().map(|x| x.to_string()).collect(), keywords: vec![] } }
/// The team as plans/task-assignments.md § Roles has it (2026-10-25), used when no note says otherwise.
pub fn builtin_roles() -> Roles {
    Roles { source: "built-in".into(), fallback: "mills".into(), people: vec![
        builtin_person("sponge", "Sponge", &["product management", "focused design and spec-driven development", "infrastructure / sysadmin", "systems direction", "prompt engineering", "debugging / testing"]),
        builtin_person("mills", "Mills", &["organisation", "treasury / secretary", "task decomposition and refinement", "accept / reject / defer", "planning", "documentation", "legal", "scope and alignment", "graphical design"]),
        builtin_person("gee", "Gee", &["creative direction", "conceptual design", "systems architecture", "planning", "research", "prompt engineering", "investor relations"]),
    ] }
}
/// The roles file: `{ "people": [{ "user", "name"?, "initial"?, "specialties": [..], "keywords"?: [..] }], "fallback"?: "mills" }` (or just the array).
pub fn roles_from_json(text: &str) -> Option<Roles> {
    // team notes are markdown: the JSON may sit in a ```json fence among the explanations
    let v: Value = serde_json::from_str(text.trim()).ok().or_else(|| { let from = text.find("```json")? + 7; let rest = &text[from..]; serde_json::from_str(rest[..rest.find("```")?].trim()).ok() })?;
    let arr = if v.is_array() { v.clone() } else { v["people"].clone() };
    let strs = |x: &Value| -> Vec<String> { x.as_array().map(|a| a.iter().filter_map(|y| y.as_str().map(|z| z.trim().to_string())).filter(|z| !z.is_empty()).collect()).unwrap_or_default() };
    let people: Vec<Role> = arr.as_array()?.iter().filter_map(|p| {
        let user = p["user"].as_str()?.trim().to_lowercase(); if user.is_empty() { return None; }
        let name = p["name"].as_str().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).unwrap_or_else(|| { let mut c = user.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() });
        let initial = p["initial"].as_str().map(|x| x.trim().to_uppercase()).filter(|x| !x.is_empty()).unwrap_or_else(|| name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default());
        Some(Role { user, name, initial, specialties: strs(&p["specialties"]), keywords: strs(&p["keywords"]) })
    }).collect();
    if people.is_empty() { return None; }
    let fb = v["fallback"].as_str().map(|x| x.trim().to_lowercase()).filter(|f| people.iter().any(|p| &p.user == f)).unwrap_or_else(|| people.iter().find(|p| p.user == "mills").map(|p| p.user.clone()).unwrap_or_else(|| people[0].user.clone()));
    Some(Roles { people, fallback: fb, source: ROLES_NOTE.into() })
}
/// The "## Roles" bullets of plans/task-assignments.md: `- **Name** (`user`): a, b, c.`
pub fn roles_from_assignments(text: &str) -> Option<Roles> {
    let start = text.find("\n## Roles")?; let rest = &text[start + 1..];
    let body = rest.split_once('\n').map(|x| x.1).unwrap_or(""); let body = body.split("\n## ").next().unwrap_or(body);
    let mut people: Vec<Role> = vec![]; let mut acc: Option<(String, String, String)> = None;
    let flush = |acc: &mut Option<(String, String, String)>, people: &mut Vec<Role>| if let Some((name, user, list)) = acc.take() {
        let specialties: Vec<String> = list.trim().trim_end_matches('.').split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        people.push(Role { initial: name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default(), user, name, specialties, keywords: vec![] });
    };
    for l in body.lines() {
        if let Some(c) = re!(r"^- \*\*([^*]+)\*\*\s*\(`([^`]+)`\):\s*(.*)$").captures(l) { flush(&mut acc, &mut people); acc = Some((c[1].trim().into(), c[2].trim().to_lowercase(), c[3].into())); }
        else if l.starts_with("  ") && acc.is_some() { if let Some(a) = acc.as_mut() { a.2.push(' '); a.2.push_str(l.trim()); } }
        else if l.trim().is_empty() { flush(&mut acc, &mut people); }
    }
    flush(&mut acc, &mut people);
    if people.is_empty() { return None; }
    let fb = people.iter().find(|p| p.user == "mills").map(|p| p.user.clone()).unwrap_or_else(|| people[0].user.clone());
    Some(Roles { people, fallback: fb, source: ASSIGNMENTS_NOTE.into() })
}
/// The person a marker names ("M", "Mills", "mills"), if the team has one.
pub fn person_by_token<'a>(roles: &'a Roles, tok: &str) -> Option<&'a Role> {
    let t = tok.trim().trim_matches(|c: char| "*#:()[]".contains(c)).trim().to_lowercase(); if t.is_empty() { return None; }
    roles.people.iter().find(|p| p.user == t || p.name.to_lowercase() == t || (t.chars().count() == 1 && p.initial.to_lowercase() == t))
}
/// The keywords a role is matched on: (stems that must all be in the text, weight). The file's own keywords count 2, the built-in
/// ones 2 when the file gives none, the words of each specialty 1.
fn role_keys(r: &Role) -> Vec<(Vec<String>, u32)> {
    let mut out: Vec<(Vec<String>, u32)> = vec![];
    let own: Vec<String> = if r.keywords.is_empty() { default_keywords(&r.user).iter().map(|x| x.to_string()).collect() } else { r.keywords.clone() };
    for k in own { let w = words(&k); if !w.is_empty() { out.push((w, 2)); } }
    for sp in &r.specialties { for w in words(sp) { if w.len() >= 4 { out.push((vec![w], 1)); } } }
    out
}

/// Each person's open tasks from board cards: counts by status and priority, the ones they hold, and a load for tie-breaks.
pub fn workload(tasks: &[Value], roles: &Roles) -> Value {
    let mut m: BTreeMap<String, Value> = BTreeMap::new();
    for p in &roles.people { m.insert(p.user.clone(), json!({ "open": 0, "active": 0, "backlog": 0, "held": 0, "byPriority": {}, "load": 0.0 })); }
    for t in tasks {
        if t["status"] == json!("done") { continue; }
        let Some(u) = t["assignee"].as_str().filter(|u| !u.is_empty()) else { continue };
        let e = m.entry(u.to_string()).or_insert_with(|| json!({ "open": 0, "active": 0, "backlog": 0, "held": 0, "byPriority": {}, "load": 0.0 }));
        let bump = |e: &mut Value, k: &str| { e[k] = json!(e[k].as_i64().unwrap_or(0) + 1); };
        bump(e, "open");
        if t["status"] == json!("backlog") { bump(e, "backlog"); } else { bump(e, "active"); }
        if t["claim"].is_object() { bump(e, "held"); }
        let p = t["priority"].as_i64().unwrap_or(0).to_string();
        e["byPriority"][&p] = json!(e["byPriority"][&p].as_i64().unwrap_or(0) + 1);
    }
    for e in m.values_mut() { e["load"] = json!(e["active"].as_f64().unwrap_or(0.0) + 0.25 * e["backlog"].as_f64().unwrap_or(0.0) + 0.5 * e["held"].as_f64().unwrap_or(0.0)); }
    json!(m)
}
fn load_of(wl: &Value, user: &str) -> f64 { wl[user]["load"].as_f64().unwrap_or(0.0) }

/// A suggested owner for an idea `{title, body, model?, person?, blockedOn?}`: an explicit marker first, then role keywords
/// (title hits count more than body hits), a tie or no match by the lighter workload; never empty ("mills to pick").
pub fn owner(idea: &Value, roles: &Roles, wl: &Value) -> Value {
    let title = s_of(idea, "title"); let body = s_of(idea, "body");
    let (tw, bw) = (wordset(&title), wordset(&body));
    let labels = suggest_labels(idea);
    let blocked = idea["blockedOn"].as_array().map(|a| !a.is_empty()).unwrap_or(false) || idea["blockedOn"].as_str().map(|x| !x.is_empty()).unwrap_or(false);
    let low = format!("{title} {body}").to_lowercase();
    let priority = if re!(r"\b(urgent|asap|right away|first|unblocks?)\b").is_match(&low) { 1 } else if blocked { 3 } else { 2 };
    let finish = |o: &Role, by: &str, reason: String, scores: Value, pick: bool| json!({ "owner": o.user, "name": o.name, "by": by, "reason": reason, "needsPick": pick, "scores": scores, "workload": { "open": wl[&o.user]["open"], "load": load_of(wl, &o.user) }, "model": idea["model"].as_str().filter(|m| !m.is_empty()), "labels": labels, "priority": priority, "blocked": blocked });
    if roles.people.is_empty() { return json!({ "owner": "mills", "name": "Mills", "by": "fallback", "reason": "no roles known: mills to pick", "needsPick": true, "scores": {}, "model": idea["model"], "labels": labels, "priority": priority, "blocked": blocked }); }
    // 1. an explicit marker
    let marker = idea["person"].as_str().or(idea["personMarker"].as_str()).unwrap_or("");
    if let Some(p) = person_by_token(roles, marker) { return finish(p, "marker", format!("marker: the input names {} (\"Person {}\")", p.name, p.initial), json!({}), false); }
    // 2. role keywords
    let mut scores: Vec<(&Role, u32, Vec<String>)> = vec![];
    for r in &roles.people {
        let (mut sc, mut hit) = (0u32, vec![]);
        for (k, w) in role_keys(r) {
            if k.iter().all(|x| tw.contains(x)) { sc += 3 * w; hit.push(k.join(" ")); } else if k.iter().all(|x| tw.contains(x) || bw.contains(x)) { sc += w; hit.push(k.join(" ")); }
        }
        hit.sort(); hit.dedup();
        scores.push((r, sc, hit));
    }
    let sj: Value = json!(scores.iter().map(|(r, s, _)| (r.user.clone(), json!(s))).collect::<BTreeMap<_, _>>());
    let top = scores.iter().map(|x| x.1).max().unwrap_or(0);
    if top > 0 {
        let tied: Vec<&(&Role, u32, Vec<String>)> = scores.iter().filter(|x| x.1 == top).collect();
        if tied.len() == 1 { let (r, _, hit) = tied[0]; return finish(r, "role", format!("role: {} covers {}", r.name, hit.iter().take(5).cloned().collect::<Vec<_>>().join(", ")), sj, false); }
        let best = tied.iter().min_by(|a, b| load_of(wl, &a.0.user).partial_cmp(&load_of(wl, &b.0.user)).unwrap_or(std::cmp::Ordering::Equal)).unwrap();
        let names = tied.iter().map(|x| format!("{} {}", x.0.name, load_of(wl, &x.0.user))).collect::<Vec<_>>().join(" / ");
        return finish(best.0, "workload", format!("tie on role between {}: {} has the lighter workload ({names})", tied.iter().map(|x| x.0.name.clone()).collect::<Vec<_>>().join(" and "), best.0.name), sj, false);
    }
    // 3. nothing matched: the lighter workload; the fallback person picks when it is even
    let mut by_load: Vec<&Role> = roles.people.iter().collect();
    by_load.sort_by(|a, b| load_of(wl, &a.user).partial_cmp(&load_of(wl, &b.user)).unwrap_or(std::cmp::Ordering::Equal));
    let fb = roles.people.iter().find(|p| p.user == roles.fallback).unwrap_or(by_load[0]);
    if by_load.len() > 1 && (load_of(wl, &by_load[0].user) - load_of(wl, &by_load[1].user)).abs() < 1e-9 { return finish(fb, "fallback", format!("no role keyword matched and the workloads are even: {} to pick", fb.name), sj, true); }
    finish(by_load[0], "workload", format!("no role keyword matched: {} has the lightest workload ({})", by_load[0].name, load_of(wl, &by_load[0].user)), sj, true)
}
/// The labels an idea suggests: the model tag, the products it names.
pub fn suggest_labels(idea: &Value) -> Vec<String> {
    let low = format!("{} {}", s_of(idea, "title"), s_of(idea, "body")).to_lowercase();
    let mut out: Vec<String> = vec![];
    if let Some(m) = idea["model"].as_str().filter(|m| !m.is_empty()) { out.push(m.to_lowercase()); }
    for (label, pats) in [("viberoller", r"vibe\s?roller"), ("hpm", r"hyper prod(uction)? man|\bhpm\b"), ("jcode", r"jcode"), ("shadow", r"shadow")] { if Regex::new(pats).map(|r| r.is_match(&low)).unwrap_or(false) { out.push(label.into()); } }
    out
}
/// `plan_roles` answer: the people with specialties and keywords, the workload, and where the roles came from.
pub fn roles_json(roles: &Roles, wl: &Value) -> Value {
    json!({ "source": roles.source, "fallback": roles.fallback,
        "people": roles.people.iter().map(|p| json!({ "user": p.user, "name": p.name, "initial": p.initial, "specialties": p.specialties,
            "keywords": if p.keywords.is_empty() { default_keywords(&p.user).iter().map(|x| json!(x)).collect::<Vec<_>>() } else { p.keywords.iter().map(|x| json!(x)).collect() }, "workload": wl[&p.user] })).collect::<Vec<_>>(),
        "otherAssignees": wl.as_object().map(|o| o.iter().filter(|(k, _)| !roles.people.iter().any(|p| &p.user == *k)).map(|(k, v)| json!({ "user": k, "workload": v })).collect::<Vec<_>>()).unwrap_or_default(),
        "order": "an explicit marker (Person M) first, then role keywords, then the lighter workload; plan_owner never answers empty" })
}

// ---------- existing work (T-276) ----------
/// Open and recently done tasks that look like `title` (+ `extra` words): the idf-weighted share of the query's words a task's title and
/// labels hold (BM25-like, a little less for long titles), at least 0.4; best first, open before done on a tie.
pub fn existing(tasks: &[Value], title: &str, extra: &[String], now_ms: i64, days: i64, limit: usize) -> Vec<Value> {
    let mut q = wordset(title); for x in extra { q.extend(wordset(x)); }
    if q.is_empty() { return vec![]; }
    let cutoff = now_ms - days.max(0) * 86_400_000;
    let docs: Vec<(&Value, BTreeSet<String>)> = tasks.iter().filter(|t| t["status"] != json!("done") || t["updatedAt"].as_i64().unwrap_or(0) >= cutoff)
        .map(|t| { let labels = t["labels"].as_array().map(|a| a.iter().filter_map(|l| l.as_str()).filter(|l| !re!(r"^T-\d+$").is_match(l)).collect::<Vec<_>>().join(" ")).unwrap_or_default(); (t, wordset(&format!("{} {}", s_of(t, "title"), labels))) }).collect();
    let n = docs.len() as f64;
    let idf = |w: &String| { let df = docs.iter().filter(|d| d.1.contains(w)).count() as f64; (1.0 + (n - df + 0.5) / (df + 0.5)).ln() };
    let weights: Vec<(String, f64)> = q.iter().map(|w| (w.clone(), idf(w))).collect();
    let total: f64 = weights.iter().map(|x| x.1).sum();
    let mut out: Vec<(f64, bool, Value)> = vec![];
    for (t, set) in &docs {
        let hit: Vec<&(String, f64)> = weights.iter().filter(|(w, _)| set.contains(w)).collect();
        if hit.is_empty() || (q.len() > 1 && hit.len() < 2) { continue; }
        let cov = hit.iter().map(|x| x.1).sum::<f64>() / total.max(1e-9);
        let score = cov * (0.75 + 0.25 * (q.len() as f64 / set.len().max(1) as f64).min(1.0));
        if score < 0.4 { continue; }
        let done = t["status"] == json!("done");
        out.push((score, done, json!({ "id": t["id"], "title": t["title"], "status": t["status"], "assignee": t["assignee"], "priority": t["priority"], "labels": t["labels"], "done": done, "kind": if re!(r"^T-\d+ · \d+/\d+").is_match(&s_of(t, "title")) { "subtask" } else { "task" },
            "score": (score * 100.0).round() / 100.0, "matched": hit.iter().map(|x| x.0.clone()).collect::<Vec<_>>(), "updatedAt": t["updatedAt"] })));
    }
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
    out.into_iter().take(limit.clamp(1, 30)).map(|x| x.2).collect()
}

// ---------- the glossary (T-278) ----------
/// The words and decisions of plans/glossary.md: `- **Term:** meaning (source)` under "## Words" and "## Decisions".
pub fn glossary(text: &str) -> Value {
    let (mut words_v, mut decisions): (Vec<Value>, Vec<Value>) = (vec![], vec![]);
    let mut section = "";
    let mut cur: Option<(String, String)> = None;
    let mut push = |section: &str, cur: &mut Option<(String, String)>| if let Some((term, body)) = cur.take() {
        let term = term.trim().trim_matches(|c: char| c == ':' || c == '.' || c == '"' || c == '\u{201c}' || c == '\u{201d}').trim().to_string();
        let mut meaning = body.replace("**", "").split_whitespace().collect::<Vec<_>>().join(" ");
        let mut source = String::new();
        if let Some(c) = re!(r"\(([^()]*)\)\s*$").captures(&meaning.clone()) { source = c[1].to_string(); meaning = meaning[..meaning.len() - c[0].len()].trim().to_string(); }
        if section == "words" {
            let aliases: Vec<String> = term.split(" / ").map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect();
            words_v.push(json!({ "term": term, "aliases": aliases, "meaning": meaning, "source": source }));
        } else if section == "decisions" { decisions.push(json!({ "title": term, "text": meaning, "source": source })); }
    };
    for l in text.lines() {
        if let Some(h) = l.strip_prefix("## ") { push(section, &mut cur); let h = h.trim().to_lowercase(); section = if h.starts_with("words") { "words" } else if h.starts_with("decisions") { "decisions" } else { "" }; continue; }
        if let Some(c) = re!(r"^- \*\*(.+?)\*\*\s*(.*)$").captures(l) { push(section, &mut cur); cur = Some((c[1].to_string(), c[2].to_string())); }
        else if l.starts_with(' ') && !l.trim().is_empty() { if let Some(c) = cur.as_mut() { c.1.push(' '); c.1.push_str(l.trim().trim_start_matches("- ")); } }
        else if l.trim().is_empty() { push(section, &mut cur); }
    }
    push(section, &mut cur);
    json!({ "source": GLOSSARY_NOTE, "words": words_v, "decisions": decisions,
        "rules": ["\"Blocked on X\" means based on X (built from it, X comes first): the task goes to backlog with the label blocked and says what it waits for.", "When a person corrects a word or a decision, add it to plans/glossary.md so it is never asked twice.", "Ask only what the glossary and the roles do not answer."] })
}
/// Used when the Notes folder has no plans/glossary.md.
pub const BUILTIN_GLOSSARY: &str = "# Team glossary\n\n## Words\n\n- **\"Blocked on X\":** means **based on X**: built from it, so X comes first. (Sponge, 2026-10-24)\n- **Hyper workflows / hyper work:** an **independent system** of our own, not Claude workflows, skills or agent scripts. (Sponge, 2026-10-24; T-184)\n- **Planner mode:** the AI only plans onto the board and never works on tasks directly. (T-273)\n- **Person S / M / G:** Sponge / Mills / Gee. A tag after `---` (opus / fable) is the suggested model.\n\n## Decisions\n\n- **Everything private by default.** Nothing public or open-sourced without Sponge's word. (plans/product-direction.md)\n- **Tasks belong to their assignee.** Every task has an owner. (plans/task-assignments.md)\n";

// ---------- parsing the input (T-276) ----------
#[derive(Clone, Debug, Default)]
struct Idea { title: String, body: String, model: Option<String>, user: Option<String>, marker: Option<String>, blocked: Vec<String>, from: usize, to: usize, form: &'static str, generated: bool, aka: Vec<String>, merged: Vec<Value>, differs: Vec<Value>, added: Vec<String> }
#[derive(Default)]
struct Block { model: Option<String>, title: Option<String>, blocked: Vec<String>, body: Vec<(usize, String)>, from: usize, to: usize, user: Option<String>, marker: Option<String> }

fn is_sep(t: &str) -> Option<Option<String>> {
    let c = re!(r"^-{3,}\s*([A-Za-z0-9._-]*)").captures(t)?;
    if t.trim_start_matches('-').trim().split_whitespace().count() > 3 { return None; }
    let tag = c[1].to_lowercase();
    Some(if ["opus", "fable", "sonnet", "haiku"].contains(&tag.as_str()) { Some(tag) } else { None })
}
fn title_line(t: &str) -> Option<String> { re!(r"^\[([^\[\]]{2,160})\]$").captures(t).map(|c| c[1].trim().to_string()) }
fn blocked_targets(t: &str) -> Option<Vec<String>> {
    let c = re!(r"(?i)^\(?\s*blocked\s+on\s*[:\-]?\s*(.+?)\s*\)?\.?\s*$").captures(t)?;
    let rest = c[1].to_string();
    let bracketed: Vec<String> = re!(r"\[([^\[\]]+)\]").captures_iter(&rest).map(|m| m[1].trim().to_string()).collect();
    if !bracketed.is_empty() { return Some(bracketed); }
    let v: Vec<String> = rest.split(',').flat_map(|x| x.split(" and ")).map(|x| x.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '\u{201c}' || c == '\u{201d}').trim().to_string()).filter(|x| !x.is_empty()).collect();
    if v.is_empty() { None } else { Some(v) }
}
fn person_line(t: &str, roles: &Roles) -> Option<String> {
    let bare = t.trim().trim_matches(|c: char| "#*: ".contains(c)).trim();
    if bare.is_empty() || bare.split_whitespace().count() > 3 { return None; }
    if let Some(c) = re!(r"(?i)^person\s+(\w+)$").captures(bare) { return Some(c[1].to_string()); }
    if roles.people.iter().any(|p| p.name.eq_ignore_ascii_case(bare) || p.user.eq_ignore_ascii_case(bare)) && t.trim().len() < 40 && !t.trim().starts_with('[') { return Some(bare.to_string()); }
    None
}
fn is_role_line(t: &str) -> bool {
    let inner = if t.starts_with('(') && t.ends_with(')') { &t[1..t.len() - 1] } else if t.starts_with('[') && t.ends_with(']') { &t[1..t.len() - 1] } else { return false };
    let low = inner.to_lowercase();
    low.starts_with("primary") || low.starts_with("role") || inner.contains(',')
}
const FILLER: &str = r"(?i)^(?:and |also,? |plus |oh,? |maybe |we |i |they |there should be |there needs to be |should |needs? |wants? |to |the ability to |ability to |a way to |a way for |capability to |the capability to |be able to |have |make sure |it would be (?:really )?(?:good|nice|great) to |really |a |an |the )";
/// A working title from the first clause of a text: filler words dropped, at most 8 words.
pub fn gen_title(text: &str) -> String {
    let first = text.trim().split(|c| ".!?;:\n".contains(c)).next().unwrap_or("").trim();
    let mut s = first.to_string();
    loop { let n = re!(FILLER).replace(&s, "").to_string(); if n == s || n.is_empty() { break; } s = n; }
    let ws: Vec<&str> = s.split_whitespace().take(8).collect();
    let mut t = ws.join(" "); t = t.trim_matches(|c: char| ",-\u{2014}\"' ".contains(c)).to_string();
    for tail in [" and", " or", " to", " the", " a", " for"] { if t.to_lowercase().ends_with(tail) { t.truncate(t.len() - tail.len()); } }
    let mut c = t.chars(); match c.next() { Some(f) => f.to_uppercase().collect::<String>() + c.as_str(), None => "Untitled idea".into() }
}
fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect(); let mut out = vec![]; let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && chars.get(i + 1).map(|n| n.is_whitespace()).unwrap_or(true) {
            let last = cur.trim_end_matches(|x: char| ".!?".contains(x)).rsplit(|x: char| x.is_whitespace()).next().unwrap_or("").to_lowercase();
            if ["e.g", "i.e", "etc", "vs", "ext", "eg", "ie", "approx"].contains(&last.as_str()) { continue; }
            if !cur.trim().is_empty() { out.push(cur.trim().to_string()); } cur.clear();
        }
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
    out
}
/// Splits a sentence at a new wish inside it (", plus maybe …", "oh and …"); a side with fewer than 4 words, or a left side ending in
/// an auxiliary ("we should also"), is not a cut.
fn split_inline(s: &str) -> Vec<String> {
    let m = re!(r"(?i)(?:[,;]\s*|\s+)(?:and\s+also|oh,?\s+and|and\s+oh|plus\s+maybe|plus|and\s+maybe|maybe|also|additionally|on\s+top\s+of\s+that)\s+");
    let mut out: Vec<String> = vec![]; let mut last = 0usize;
    for x in m.find_iter(s) {
        let left = &s[last..x.start()];
        let lw: Vec<&str> = left.split_whitespace().collect();
        let aux = lw.last().map(|w| ["should", "must", "can", "will", "could", "would", "to", "and", "be", "is", "are", "we", "i", "they", "also"].contains(&w.to_lowercase().as_str())).unwrap_or(true);
        if lw.len() < 4 || aux || s[x.end()..].split_whitespace().count() < 3 { continue; }
        out.push(left.trim().to_string()); last = x.end();
    }
    out.push(s[last..].trim().to_string());
    out
}
fn starts_wish(s: &str) -> bool {
    if re!(r"(?i)^(?:oh,?\s+and|and\s+also|and\s+maybe)\b").is_match(s) { return true; }
    re!(r"(?i)^(?:and\s+|oh,?\s+(?:and\s+)?)?(?:(?:also|plus|maybe|additionally|another(?:\s+(?:one|thing))?|on top of that|then there'?s|next)\b|(?:we|i)\s+(?:also\s+|really\s+)?(?:need|want|should|must|would like|would love|could use)\b|(?:add|make|build|create|let'?s|please add|can we|could we|it would be|there should|there needs)\b)").is_match(s.trim_start_matches(|c: char| c == '-' || c == '*' || c.is_whitespace()))
}
/// Form D (and the paragraphs of form B): run-on text into (idea text, first line, last line) plus what is not an idea.
fn split_runon(lines: &[(usize, String)]) -> (Vec<(String, usize, usize)>, Vec<(String, usize, &'static str)>) {
    // paragraphs: blank lines and bullets break them
    let mut paras: Vec<(String, usize, usize)> = vec![];
    for (no, l) in lines {
        let t = l.trim();
        if t.is_empty() { paras.push((String::new(), *no, *no)); continue; }
        let bullet = re!(r"^(?:[-*\u{2022}]|\d+[.)])\s+").is_match(t);
        let t2 = re!(r"^(?:[-*\u{2022}]|\d+[.)])\s+").replace(t, "").to_string();
        match paras.last_mut() { Some(p) if !p.0.is_empty() && !bullet => { p.0.push(' '); p.0.push_str(&t2); p.2 = *no; } _ => paras.push((t2, *no, *no)) }
    }
    let (mut ideas, mut left): (Vec<(String, usize, usize)>, Vec<(String, usize, &'static str)>) = (vec![], vec![]);
    for (text, from, to) in paras.into_iter().filter(|p| !p.0.is_empty()) {
        for (si, sent) in split_sentences(&text).into_iter().enumerate() {
            let sent = re!(r"(?i)^(?:hi|hey|hello|yo)\b[ ,!]*(?:\w+\b[,!]?\s*)?").replace(&sent, "").to_string();
            let pieces = split_inline(&sent);
            for (pi, piece) in pieces.into_iter().enumerate() {
                let piece = piece.trim().to_string();
                if piece.split_whitespace().count() < 3 || re!(r"(?i)^(?:thanks|thank you|cheers|ok(?:ay)?|anyway|that'?s all|that is all|does that make sense|let me know|any questions|please)\b").is_match(&piece) { if !piece.is_empty() { left.push((piece, from, "chat, not an idea")); } continue; }
                let new = pi > 0 || ideas.is_empty() || (si == 0 && !ideas.is_empty() && starts_wish(&piece)) || (si > 0 && starts_wish(&piece));
                if new { ideas.push((piece, from, to)); } else if let Some(l) = ideas.last_mut() { l.0.push(' '); l.0.push_str(&piece); l.2 = to; }
            }
        }
    }
    (ideas, left)
}
fn finish_idea(i: &mut Idea) { i.body = i.body.trim().to_string(); if i.title.is_empty() { i.title = gen_title(&i.body); i.generated = true; } i.aka = vec![i.title.clone()]; }

/// plan_parse: the input split into ideas. See the module text for the forms; every idea has `sourceSpan` (first / last line of the input).
pub fn parse(text: &str, roles: &Roles) -> Value {
    let raw: String = text.chars().take(60_000).collect();
    let lines: Vec<(usize, String)> = raw.replace("\r\n", "\n").replace('\r', "\n").split('\n').enumerate().map(|(i, l)| (i + 1, l.trim_end().to_string())).filter(|(_, l)| !l.trim_start().starts_with("```")).collect();
    let explicit_any = lines.iter().any(|(_, l)| is_sep(l.trim()).is_some() || title_line(l.trim()).is_some());
    let person_any = lines.iter().any(|(_, l)| person_line(l, roles).is_some());
    let mut ideas: Vec<Idea> = vec![]; let mut left: Vec<Value> = vec![]; let mut sections: Vec<Value> = vec![];
    let mut forms: BTreeSet<&str> = BTreeSet::new();
    let mut person: (Option<String>, Option<String>) = (None, None); // (username if known, marker as written)
    let mut cur: Option<Block> = None; let mut pool: Vec<(usize, String)> = vec![]; let mut pending_role = false; let mut sec_idx: Option<usize> = None;
    let note_left = |left: &mut Vec<Value>, text: &str, line: usize, why: &str| left.push(json!({ "text": text, "line": line, "reason": why }));

    macro_rules! close { () => { if let Some(b) = cur.take() {
        let mut i = Idea { title: b.title.unwrap_or_default(), body: b.body.iter().map(|x| x.1.clone()).collect::<Vec<_>>().join("\n"), model: b.model, user: b.user, marker: b.marker, blocked: b.blocked, from: b.from, to: b.to.max(b.from), form: if b.from > 0 { "A" } else { "B" }, ..Default::default() };
        if i.title.is_empty() && i.body.trim().is_empty() { } else { forms.insert("A"); finish_idea(&mut i); ideas.push(i); }
    } } }
    macro_rules! flush_pool { () => { if !pool.is_empty() {
        let p = std::mem::take(&mut pool);
        let words_n: usize = p.iter().map(|x| x.1.split_whitespace().count()).sum();
        if explicit_any && (person.1.is_some() || words_n < 8) { for (no, l) in p.iter().filter(|x| !x.1.trim().is_empty()) { note_left(&mut left, l.trim(), *no, "text outside any idea block"); } }
        else if person_any && person.1.is_some() {
            // form B: one idea per bullet / paragraph under a person
            let mut paras: Vec<Vec<(usize, String)>> = vec![vec![]];
            for (no, l) in p { let t = l.trim().to_string(); if t.is_empty() { if !paras.last().unwrap().is_empty() { paras.push(vec![]); } } else if re!(r"^(?:[-*\u{2022}]|\d+[.)])\s+").is_match(&t) && !paras.last().unwrap().is_empty() { paras.push(vec![(no, t)]); } else { paras.last_mut().unwrap().push((no, t)); } }
            for para in paras.into_iter().filter(|x| !x.is_empty()) {
                let body = para.iter().map(|x| re!(r"^(?:[-*\u{2022}]|\d+[.)])\s+").replace(&x.1, "").to_string()).collect::<Vec<_>>().join(" ");
                if body.split_whitespace().count() < 3 { note_left(&mut left, &body, para[0].0, "too short to be an idea"); continue; }
                let mut i = Idea { body, user: person.0.clone(), marker: person.1.clone(), from: para[0].0, to: para.last().unwrap().0, form: "B", ..Default::default() };
                forms.insert("B"); finish_idea(&mut i); ideas.push(i);
            }
        } else {
            let (ids, lf) = split_runon(&p);
            for (b, f, t) in ids { let mut i = Idea { body: b, user: person.0.clone(), marker: person.1.clone(), from: f, to: t, form: "D", ..Default::default() }; forms.insert("D"); finish_idea(&mut i); ideas.push(i); }
            for (t, l, why) in lf { note_left(&mut left, &t, l, why); }
        }
    } } }

    for (no, txt) in &lines {
        let t = txt.trim();
        if let Some(tok) = person_line(txt, roles) {
            flush_pool!(); close!();
            let r = person_by_token(roles, &tok);
            person = (r.map(|r| r.user.clone()), Some(tok.clone()));
            sections.push(json!({ "marker": tok, "person": r.map(|r| r.user.clone()), "line": no, "known": r.is_some() })); sec_idx = Some(sections.len() - 1);
            note_left(&mut left, t, *no, "person marker (owner of the ideas below)"); pending_role = true; forms.insert("B"); continue;
        }
        if pending_role { if t.is_empty() { continue; } pending_role = false;
            if is_role_line(t) {
                let inner = t[1..t.len() - 1].to_string(); let inner = re!(r"(?i)^(?:primary|role)s?\s*:\s*").replace(&inner, "").to_string();
                let items: Vec<String> = inner.split(|c| c == ',' || c == ';').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                let spec = person.0.as_ref().and_then(|u| roles.people.iter().find(|p| &p.user == u)).map(|p| p.specialties.join(" ")).unwrap_or_default();
                let sw = wordset(&spec);
                let (mut ok, mut missing) = (vec![], vec![]);
                for it in &items { if wordset(it).iter().any(|w| sw.contains(w)) { ok.push(it.clone()); } else { missing.push(it.clone()); } }
                let mismatch = person.0.is_none() || ok.len() * 2 < items.len();
                if let Some(i) = sec_idx { sections[i]["roleLine"] = json!(inner); sections[i]["matched"] = json!(ok); sections[i]["unmatched"] = json!(missing); sections[i]["mismatch"] = json!(mismatch); }
                note_left(&mut left, t, *no, "role line (checked against the roles file)"); continue;
            } }
        if let Some(tag) = is_sep(t) { flush_pool!(); close!(); cur = Some(Block { model: tag, from: *no, to: *no, user: person.0.clone(), marker: person.1.clone(), ..Default::default() }); continue; }
        if let Some(ti) = title_line(t) {
            match cur.as_mut() { Some(b) if b.title.is_none() && b.body.iter().all(|x| x.1.trim().is_empty()) => { b.title = Some(ti); b.to = *no; }
                _ => { flush_pool!(); close!(); cur = Some(Block { title: Some(ti), from: *no, to: *no, user: person.0.clone(), marker: person.1.clone(), ..Default::default() }); } }
            continue;
        }
        if let Some(b) = cur.as_mut() {
            if b.body.iter().all(|x| x.1.trim().is_empty()) { if let Some(tg) = blocked_targets(t) { b.blocked.extend(tg); b.to = *no; continue; } }
            b.body.push((*no, txt.clone())); if !t.is_empty() { b.to = *no; }
            continue;
        }
        pool.push((*no, txt.clone()));
    }
    flush_pool!(); close!();

    // form C: the same idea twice (a short and a long wording) is one idea
    let before = ideas.len();
    let ideas = merge(ideas);
    if ideas.len() < before { forms.insert("C"); }
    // links: "Blocked on [Other]" names another idea of the input
    let out_ideas: Vec<Value> = ideas.iter().enumerate().map(|(k, i)| {
        let links: Vec<Value> = i.blocked.iter().map(|b| { let (nb, sb) = (norm_title(b), wordset(b)); let at = ideas.iter().enumerate().find(|(j, o)| *j != k && o.aka.iter().any(|a| norm_title(a) == nb || jaccard(&wordset(a), &sb) >= 0.6)).map(|(j, _)| j + 1); json!({ "title": b, "n": at }) }).collect();
        let mut o = json!({ "n": k + 1, "title": i.title, "body": i.body, "model": i.model, "person": i.user, "personMarker": i.marker, "blockedOn": i.blocked, "blockedOnIdeas": links.iter().filter_map(|l| l["n"].as_u64()).collect::<Vec<_>>(), "blockedLinks": links,
            "sourceSpan": { "from": i.from, "to": i.to }, "form": i.form, "titleGenerated": i.generated });
        if !i.merged.is_empty() { o["merged"] = json!(i.merged); }
        if !i.differs.is_empty() { o["differs"] = json!(i.differs); }
        if !i.added.is_empty() { o["addedFromShort"] = json!(i.added); }
        o
    }).collect();
    let nmerged = before - ideas.len();
    json!({ "ideas": out_ideas, "leftovers": left, "people": sections, "forms": forms.into_iter().collect::<Vec<_>>(), "counts": { "ideas": out_ideas.len(), "merged": nmerged, "leftovers": left.len() },
        "note": "Nothing is dropped: every leftover is a marker, a role line or chat; every other line is in an idea. Check titles marked titleGenerated, owners (plan_owner) and blockedOn (means: based on) before drafting." })
}

fn merge(ideas: Vec<Idea>) -> Vec<Idea> {
    let sets: Vec<BTreeSet<String>> = ideas.iter().map(|i| wordset(&format!("{} {}", i.title, i.body))).collect();
    let n = ideas.len() as f64;
    let mut df: HashMap<&String, f64> = HashMap::new(); for s in &sets { for w in s { *df.entry(w).or_insert(0.0) += 1.0; } }
    let vec_of = |s: &BTreeSet<String>| -> HashMap<String, f64> { s.iter().map(|w| (w.clone(), (1.0 + n / df.get(w).copied().unwrap_or(1.0)).ln())).collect() };
    let cos = |a: &BTreeSet<String>, b: &BTreeSet<String>| { let (va, vb) = (vec_of(a), vec_of(b)); let dot: f64 = va.iter().filter_map(|(k, x)| vb.get(k).map(|y| x * y)).sum(); let na: f64 = va.values().map(|x| x * x).sum::<f64>().sqrt(); let nb: f64 = vb.values().map(|x| x * x).sum::<f64>().sqrt(); if na * nb == 0.0 { 0.0 } else { dot / (na * nb) } };
    let similar = |a: usize, b: usize, ia: &Idea, ib: &Idea| -> bool {
        if ia.user.is_some() && ib.user.is_some() && ia.user != ib.user { return false; }
        if norm_title(&ia.title) == norm_title(&ib.title) && !ia.generated && !ib.generated { return true; }
        let (ta, tb) = (wordset(&ia.title), wordset(&ib.title));
        if ta.len().min(tb.len()) >= 2 && jaccard(&ta, &tb) >= 0.6 { return true; }
        let (small, big) = if sets[a].len() <= sets[b].len() { (&sets[a], &sets[b]) } else { (&sets[b], &sets[a]) };
        let cont = small.intersection(big).count() as f64 / small.len().max(1) as f64;
        small.len() >= 5 && cont >= 0.75 && cos(&sets[a], &sets[b]) >= 0.3
    };
    let mut kept: Vec<(usize, Idea)> = vec![];
    for (j, i) in ideas.into_iter().enumerate() {
        let at = kept.iter().position(|(k, o)| similar(*k, j, o, &i));
        let Some(p) = at else { kept.push((j, i)); continue };
        let (k, o) = kept[p].clone();
        let (long, short) = if o.body.len() >= i.body.len() { (o, i) } else { (i, o) };
        let mut m = long.clone();
        m.merged.push(json!({ "title": short.title, "sourceSpan": { "from": short.from, "to": short.to }, "model": short.model, "kept": "the longer wording" }));
        m.merged.extend(short.merged.clone());
        if let (Some(a), Some(b)) = (&long.model, &short.model) { if a != b { m.differs.push(json!({ "field": "model", "kept": a, "other": b, "otherSpan": { "from": short.from, "to": short.to } })); } }
        if m.model.is_none() { m.model = short.model.clone(); }
        if m.user.is_none() { m.user = short.user.clone(); m.marker = short.marker.clone(); }
        for b in &short.blocked { if !m.blocked.iter().any(|x| norm_title(x) == norm_title(b)) { m.blocked.push(b.clone()); } }
        for a in &short.aka { if !m.aka.contains(a) { m.aka.push(a.clone()); } }
        // what only the short version says is added
        let lw = wordset(&long.body);
        for s in split_sentences(&short.body) { let sw = wordset(&s); if !sw.is_empty() && (sw.intersection(&lw).count() as f64 / sw.len() as f64) < 0.5 { m.added.push(s.clone()); m.body.push_str(&format!("\n\nFrom the shorter version: {s}")); } }
        kept[p] = (k, m);
    }
    kept.into_iter().map(|x| x.1).collect()
}

// ---------- the hub side (behind PlanHub: the hub's own stores, or a signed-in hub's routes) ----------
fn note_text(hub: &dyn PlanHub, path: &str) -> Option<String> { hub.note_read(path) }
/// The roles the team has now: the roles file, else § Roles of task-assignments, else the built-in list.
pub fn load_roles(hub: &dyn PlanHub) -> Roles {
    note_text(hub, ROLES_NOTE).and_then(|t| roles_from_json(&t))
        .or_else(|| note_text(hub, ASSIGNMENTS_NOTE).and_then(|t| roles_from_assignments(&t)))
        .unwrap_or_else(builtin_roles)
}
fn board_tasks(hub: &dyn PlanHub) -> Vec<Value> {
    let r = hub.board("list", &json!({}));
    r["result"]["tasks"].as_array().cloned().unwrap_or_default()
}
/// The five planner tools of the team MCP server (any session may call them: they only read). `None` = not one of them.
/// `now_ms` is the clock (for "done within N days").
pub fn tool(hub: &dyn PlanHub, name: &str, args: &Value, now_ms: i64) -> Option<Value> {
    // plan_draft is not a helper: the Work relay handles it (T-279), so it passes through here
    if !name.starts_with("plan_") || name == "plan_draft" { return None; }
    let err = |m: &str| json!({ "ok": false, "error": m });
    Some(match name {
        "plan_parse" => {
            let text = args["text"].as_str().unwrap_or("");
            if text.trim().is_empty() { return Some(err("plan_parse needs the input text.")); }
            json!({ "ok": true, "result": parse(text, &load_roles(hub)) })
        }
        "plan_existing" => {
            let title = args["title"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() { return Some(err("plan_existing needs a title.")); }
            let extra: Vec<String> = match &args["words"] { Value::String(s) => vec![s.clone()], Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(), _ => vec![] };
            let (now, days) = (now_ms, args["days"].as_i64().unwrap_or(30));
            let tasks = board_tasks(hub);
            let found = existing(&tasks, &title, &extra, now, days, args["limit"].as_u64().unwrap_or(8) as usize);
            json!({ "ok": true, "result": { "title": title, "searched": tasks.len(), "doneWithinDays": days, "matches": found, "hint": if found.is_empty() { "nothing on the board looks like it: plan it whole (still check the code and notes)" } else { "scope the task to what is left and list these under Already there; a match that is done may mean the idea is already done" } } })
        }
        "plan_roles" => {
            let roles = load_roles(hub);
            json!({ "ok": true, "result": roles_json(&roles, &workload(&board_tasks(hub), &roles)) })
        }
        "plan_owner" => {
            let roles = load_roles(hub); let wl = workload(&board_tasks(hub), &roles);
            let one = |v: &Value| if v.is_string() { json!({ "title": v, "body": "" }) } else { v.clone() };
            if let Some(a) = args["ideas"].as_array() { json!({ "ok": true, "result": { "source": roles.source, "owners": a.iter().take(60).map(|i| { let i = one(i); let mut o = owner(&i, &roles, &wl); o["title"] = i["title"].clone(); o }).collect::<Vec<_>>() } }) }
            else if args["idea"].is_object() || args["idea"].is_string() { let i = one(&args["idea"]); let mut o = owner(&i, &roles, &wl); o["source"] = json!(roles.source); json!({ "ok": true, "result": o }) }
            else { err("plan_owner needs an idea {title, body?, model?, person?, blockedOn?} (or ideas: [..]).") }
        }
        "plan_glossary" => {
            let t = note_text(hub, GLOSSARY_NOTE);
            let mut g = glossary(t.as_deref().unwrap_or(BUILTIN_GLOSSARY));
            if t.is_none() { g["source"] = json!("built-in (the Notes folder has no plans/glossary.md)"); }
            json!({ "ok": true, "result": g })
        }
        _ => err(&format!("Unknown planner tool: {name}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const EXAMPLE: &str = include_str!("planner-example.md");
    fn ideas(v: &Value) -> Vec<Value> { v["ideas"].as_array().cloned().unwrap_or_default() }
    fn by_title<'a>(v: &'a [Value], part: &str) -> &'a Value { v.iter().find(|i| i["title"].as_str().unwrap_or("").contains(part)).unwrap_or_else(|| panic!("no idea {part}")) }

    #[test]
    fn the_worked_example_is_split_into_its_eleven_ideas_with_owners_models_and_blocks() {
        let p = parse(EXAMPLE, &builtin_roles()); let v = ideas(&p);
        assert_eq!(v.len(), 11, "{:#?}", v.iter().map(|i| i["title"].clone()).collect::<Vec<_>>());
        let q = by_title(&v, "Queued Messages");
        assert_eq!((q["person"].clone(), q["model"].clone(), q["form"].clone()), (json!("mills"), json!("opus"), json!("A")));
        assert!(q["body"].as_str().unwrap().starts_with("With hyper prod man"));
        assert_eq!(by_title(&v, "Website Builder")["model"], json!("fable"));
        assert_eq!(by_title(&v, "Hyper Workflows Architecture")["person"], json!("gee"));
        assert_eq!(by_title(&v, "REA Project")["person"], json!("sponge"));
        // the blocked set: builder, triggers, delegation, REA are all based on the hyper workflows idea
        let hw = by_title(&v, "Hyper Workflows Architecture")["n"].as_u64().unwrap();
        let blocked: Vec<String> = v.iter().filter(|i| !i["blockedOn"].as_array().unwrap().is_empty()).map(|i| i["title"].as_str().unwrap().to_string()).collect();
        assert_eq!(blocked.len(), 4, "{blocked:?}");
        for b in v.iter().filter(|i| !i["blockedOn"].as_array().unwrap().is_empty()) { assert_eq!(b["blockedOnIdeas"], json!([hw])); }
        assert!(by_title(&v, "Local Ollama")["blockedOn"].as_array().unwrap().is_empty());
        // source lines point at the real text
        let span = &q["sourceSpan"]; assert!(span["from"].as_u64().unwrap() < span["to"].as_u64().unwrap());
        assert_eq!(p["counts"]["merged"], json!(0));
        // the person sections and their role lines: all three match the roles file
        let people = p["people"].as_array().unwrap(); assert_eq!(people.len(), 3);
        assert!(people.iter().all(|x| x["known"] == json!(true) && x["mismatch"] == json!(false)), "{people:?}");
        // nothing is lost: the leftovers are only markers and role lines
        assert!(p["leftovers"].as_array().unwrap().iter().all(|l| l["reason"].as_str().unwrap().contains("marker") || l["reason"].as_str().unwrap().contains("role line")));
        // a mismatching role line is flagged
        let odd = parse("Person M\n(Primary: kernel hacking, quantum plumbing)\n\n--- opus\n[X thing]\nbody of it here\n", &builtin_roles());
        assert_eq!(odd["people"][0]["mismatch"], json!(true));
    }
    #[test]
    fn owners_for_the_worked_example_follow_the_boards_assignments() {
        let roles = builtin_roles(); let wl = workload(&[], &roles);
        let v = ideas(&parse(EXAMPLE, &roles));
        let expect = [("Queued Messages", "mills"), ("Right-Click", "mills"), ("Archiving", "mills"), ("Website Builder", "mills"), ("Hyper Workflows Architecture", "gee"), ("Ollama", "gee"), ("Triggers", "gee"), ("Auto-Delegation", "gee"), ("Auto-Updater", "sponge"), ("Question UI", "sponge"), ("REA Project", "sponge")];
        for (t, who) in expect { let o = owner(by_title(&v, t), &roles, &wl); assert_eq!((o["owner"].as_str().unwrap(), o["by"].as_str().unwrap()), (who, "marker"), "{t}"); }
        assert_eq!(owner(by_title(&v, "Website Builder"), &roles, &wl)["labels"], json!(["fable", "viberoller", "hpm"]));
        assert_eq!(owner(by_title(&v, "Triggers"), &roles, &wl)["priority"], json!(3));
    }
    #[test]
    fn a_short_and_a_long_version_are_merged_and_the_differences_noted() {
        let short = "Person M\n\n--- fable\n[Board Archiving]\nArchive done tasks after about 48 hours, delete the archived after 30 days.\n\n--- opus\n[Right Click Menus]\nCustom context menus on the web.\n";
        let long = "--- opus\n[Automated Task Board Archiving System]\n\nWe need the ability for the task boards to be able to archive tasks, and delete tasks that have been archived after ~30 days. Done tasks should auto archive after ~48 hours, but can be manually or AI archived. Also a done filter.\n";
        let p = parse(&format!("{short}\n{long}"), &builtin_roles()); let v = ideas(&p);
        assert_eq!(v.len(), 2, "{v:#?}");
        let a = by_title(&v, "Archiving");
        assert_eq!(a["title"], json!("Automated Task Board Archiving System"));
        assert_eq!(a["model"], json!("opus"));
        assert_eq!(a["differs"][0]["other"], json!("fable"));
        assert_eq!(a["merged"][0]["title"], json!("Board Archiving"));
        assert_eq!(a["person"], json!("mills"), "the person comes from the short version");
        assert_eq!(p["counts"]["merged"], json!(1));
        assert!(p["forms"].as_array().unwrap().contains(&json!("C")));
        // two different ideas that share some words are not merged
        let two = parse("--- opus\n[Queued Messages Panel]\nShow queued messages in a sidebar panel.\n\n--- opus\n[Messages Search]\nSearch all sent messages by word.\n", &builtin_roles());
        assert_eq!(ideas(&two).len(), 2);
    }
    #[test]
    fn run_on_chat_is_split_at_each_new_wish() {
        let t = "Hey Mills, we need a board archive so the done tab stops filling up. The done tab is filling up too much. Also we should have a right-click menu in the web version, plus maybe a queued messages panel. Oh and the board should show who is online. Thanks!";
        let p = parse(t, &builtin_roles()); let v = ideas(&p);
        let titles: Vec<String> = v.iter().map(|i| i["title"].as_str().unwrap().to_string()).collect();
        assert_eq!(v.len(), 4, "{titles:?}");
        assert!(v[0]["body"].as_str().unwrap().contains("filling up too much"), "a detail sentence stays with its idea");
        assert!(v.iter().all(|i| i["form"] == json!("D") && i["titleGenerated"] == json!(true)));
        assert!(titles[1].to_lowercase().contains("right-click") && titles[2].to_lowercase().contains("queued") && titles[3].to_lowercase().contains("board should"), "{titles:?}");
        assert!(p["leftovers"].as_array().unwrap().iter().any(|l| l["text"] == json!("Thanks!")));
        // an idea of a person section without separators
        let b = parse("Person G\n\n- research the local ollama detection on Windows\n- write the investor update for November\n", &builtin_roles());
        assert_eq!(ideas(&b).len(), 2); assert_eq!(ideas(&b)[1]["person"], json!("gee")); assert_eq!(ideas(&b)[0]["form"], json!("B"));
    }
    #[test]
    fn existing_work_is_found_by_word_overlap_open_and_recently_done() {
        let now = 1_800_000_000_000i64; let day = 86_400_000i64;
        let t = |id: &str, title: &str, status: &str, age: i64| json!({ "id": id, "title": title, "status": status, "assignee": "gee", "priority": 2, "labels": ["T-184"], "updatedAt": now - age * day });
        let tasks = vec![
            t("T-198", "T-184 · 1/10: Hyper workflows spec: node-based, independent system", "doing", 1),
            t("T-188", "Queued messages sidebar panel", "done", 3),
            t("T-100", "Queued messages sidebar panel (old attempt)", "done", 90),
            t("T-300", "Presence and activity for the team", "todo", 1),
            t("T-301", "Website builder for the AI", "backlog", 1),
        ];
        let m = existing(&tasks, "Node-Based Hyper Workflows Architecture", &[], now, 30, 8);
        assert_eq!(m[0]["id"], json!("T-198"), "{m:?}");
        assert!(!m.iter().any(|x| x["id"] == json!("T-300")));
        let q = existing(&tasks, "Hidable Queued Messages Sidebar Panel", &["view queued messages".into()], now, 30, 8);
        assert_eq!(q.iter().map(|x| x["id"].as_str().unwrap()).collect::<Vec<_>>(), ["T-188"], "the 90 day old done task is out of the window");
        assert_eq!(q[0]["done"], json!(true));
        assert!(existing(&tasks, "Secure Gated Product Release Downloads", &[], now, 30, 8).is_empty());
        assert_eq!(existing(&tasks, "", &[], now, 30, 8).len(), 0);
    }
    #[test]
    fn roles_come_from_a_json_file_else_the_assignments_note_else_built_in() {
        let r = roles_from_json(ROLES_EXAMPLE).expect("the example file parses");
        assert_eq!(r.people.iter().map(|p| p.user.as_str()).collect::<Vec<_>>(), ["sponge", "mills", "gee"]);
        assert_eq!((r.fallback.as_str(), r.source.as_str()), ("mills", ROLES_NOTE));
        assert!(r.people[0].keywords.contains(&"hub".to_string()));
        let note = "# T\n\n## Roles\n\n- **Sponge** (`sponge`): product management, infrastructure / sysadmin,\n  debugging / testing.\n- **Ann** (`ann`): illustration, UI design.\n\n## Board rules\n\n1. x\n";
        let a = roles_from_assignments(note).unwrap();
        assert_eq!(a.people.len(), 2); assert_eq!(a.people[0].specialties, ["product management", "infrastructure / sysadmin", "debugging / testing"]);
        assert_eq!(a.people[1].initial, "A");
        assert_eq!(roles_from_json(&format!("# Roles\n\nwords\n\n```json\n{ROLES_EXAMPLE}\n```\n\nmore")).unwrap().people.len(), 3, "a json fence in a markdown note");
        assert!(roles_from_json("not json").is_none() && roles_from_json("{\"people\":[]}").is_none());
        assert_eq!(builtin_roles().people.len(), 3);
        // the team's own task-assignments note parses to the same three people
        let real = "## Roles\n\n- **Sponge** (`sponge`): product management, focused design and spec-driven development.\n- **Mills** (`mills`): organisation, planning.\n- **Gee** (`gee`): creative direction, research.\n";
        assert_eq!(roles_from_assignments(&format!("\n{real}")).unwrap().people.len(), 3);
    }
    #[test]
    fn an_owner_is_a_marker_then_role_keywords_then_the_lighter_workload_and_never_empty() {
        let roles = roles_from_json(ROLES_EXAMPLE).unwrap();
        let task = |u: &str, st: &str, p: i64, claim: bool| json!({ "assignee": u, "status": st, "priority": p, "claim": if claim { json!({ "by": "x" }) } else { Value::Null } });
        let wl = workload(&[task("mills", "todo", 2, false), task("mills", "doing", 1, true), task("mills", "backlog", 4, false), task("gee", "todo", 2, false), task("mills", "done", 2, false)], &roles);
        assert_eq!(wl["mills"]["open"], json!(3)); assert_eq!(wl["mills"]["held"], json!(1)); assert_eq!(wl["mills"]["byPriority"]["2"], json!(1)); assert_eq!(wl["mills"]["load"], json!(2.75)); assert_eq!(wl["sponge"]["open"], json!(0));
        let o = |idea: Value| owner(&idea, &roles, &wl);
        assert_eq!(o(json!({ "title": "Sidebar panel", "person": "G" }))["owner"], json!("gee"));
        assert_eq!(o(json!({ "title": "Sidebar panel", "person": "G" }))["by"], json!("marker"));
        let r = o(json!({ "title": "Hub protocol relay tests", "body": "debug the server" })); assert_eq!((r["owner"].clone(), r["by"].clone()), (json!("sponge"), json!("role")));
        let r = o(json!({ "title": "Redesign the sidebar menu layout" })); assert_eq!(r["owner"], json!("mills"));
        let r = o(json!({ "title": "Research node architecture for workflows" })); assert_eq!(r["owner"], json!("gee"));
        // a tie goes to the lighter workload: "planning" is in Mills' and Gee's specialties, Gee has fewer open tasks
        let r = o(json!({ "title": "Planning", "body": "" })); assert_eq!((r["owner"].clone(), r["by"].clone()), (json!("gee"), json!("workload")), "{r}");
        // nothing matches: the lightest workload picks, flagged for a person to confirm; an unknown marker is ignored
        let r = o(json!({ "title": "Zzz qqq", "person": "Nobody" })); assert_eq!(r["owner"], json!("sponge")); assert_eq!(r["needsPick"], json!(true));
        // even workloads and no match: mills to pick
        let even = workload(&[], &roles); let r = owner(&json!({ "title": "Zzz qqq" }), &roles, &even); assert_eq!((r["owner"].clone(), r["by"].clone()), (json!("mills"), json!("fallback"))); assert!(r["reason"].as_str().unwrap().contains("to pick"));
        assert_eq!(owner(&json!({ "title": "x" }), &Roles { people: vec![], fallback: "mills".into(), source: "x".into() }, &json!({}))["owner"], json!("mills"));
        assert_eq!(o(json!({ "title": "Urgent: unblock the release" }))["priority"], json!(1));
    }
    #[test]
    fn the_five_tools_run_over_a_plan_hub_and_never_write() {
        let hub = crate::testkit::FakeHub::default();
        hub.tasks.borrow_mut().push(json!({ "id": "T-9", "title": "Hyper workflows spec", "status": "todo", "assignee": "gee", "priority": 2 }));
        let call = |n: &str, a: Value| tool(&hub, n, &a, 0).unwrap_or_else(|| panic!("{n}"));
        assert_eq!(call("plan_parse", json!({ "text": EXAMPLE }))["result"]["ideas"].as_array().map(|a| a.len()), Some(11));
        assert_eq!(call("plan_parse", json!({ "text": " " }))["ok"], json!(false));
        assert_eq!(call("plan_roles", json!({}))["result"]["people"].as_array().map(|a| a.len()), Some(3));
        assert_eq!(call("plan_owner", json!({ "idea": "Hub protocol relay tests" }))["result"]["owner"], json!("sponge"));
        assert_eq!(call("plan_owner", json!({}))["ok"], json!(false));
        assert_eq!(call("plan_existing", json!({ "title": "Hyper workflows spec" }))["result"]["searched"], json!(1));
        assert!(call("plan_glossary", json!({}))["result"]["source"].as_str().unwrap().starts_with("built-in"));
        hub.notes.borrow_mut().insert(GLOSSARY_NOTE.into(), "# G\n\n## Words\n\n- **Zed:** a letter.\n".into());
        assert_eq!(call("plan_glossary", json!({}))["result"]["words"][0]["term"], json!("Zed"));
        assert!(tool(&hub, "plan_draft", &json!({}), 0).is_none() && tool(&hub, "team_say", &json!({}), 0).is_none());
        assert!(hub.calls.borrow().iter().all(|c| c.0 == "list"), "read only");
    }
    #[test]
    fn the_glossary_note_is_read_into_words_and_decisions() {
        let g = glossary("# Team glossary\n\n## Words\n\n- **Hyper workflows / hyper work:** an **independent system** of our own.\n  - A re-usable flow chart. (Sponge, 2026-10-24; T-184)\n- **\"Blocked on X\":** means **based on X**: built from it. (Sponge, 2026-10-24)\n\n## Decisions\n\n- **Everything private by default.** Built for the team first. (Sponge, 2026-10-24)\n");
        let w = g["words"].as_array().unwrap(); assert_eq!(w.len(), 2);
        assert_eq!(w[0]["aliases"], json!(["hyper workflows", "hyper work"]));
        assert!(w[0]["meaning"].as_str().unwrap().contains("re-usable flow chart") && w[0]["source"] == json!("Sponge, 2026-10-24; T-184"));
        assert_eq!(w[1]["term"], json!("Blocked on X")); assert!(w[1]["meaning"].as_str().unwrap().starts_with("means based on X"));
        assert_eq!(g["decisions"][0]["title"], json!("Everything private by default")); assert_eq!(g["decisions"][0]["text"], json!("Built for the team first."));
        let built = glossary(BUILTIN_GLOSSARY); assert!(built["words"].as_array().unwrap().len() >= 4 && built["decisions"].as_array().unwrap().len() == 2);
        assert!(g["rules"][0].as_str().unwrap().contains("based on X"));
    }
}
