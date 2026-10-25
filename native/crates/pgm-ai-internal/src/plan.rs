//! Planner mode: draft, approve, write (T-279 / T-281; team note plans/planner-mode.md § 4.7, 5 rules 7, 12, 13 and § 6).
//! The planner AI never writes to the board. It sends the whole plan as a draft (the team MCP tool `plan_draft`); the
//! person approves all, some or none on their page; and only then does the HUB write it, in the order of rule 12: the
//! source note, the parents, the sub-tasks, the changes to existing tasks. Then the summary goes to the chat once and the
//! owner map is appended to the plan note (T-281). This file is the pure part (checking the draft) and the writing; the
//! open drafts, their events and the waiting session are in work.rs (`Work::plan_draft`, `Work::plan_answer`).
use pgm_core_internal::json::s;
use pgm_identity_internal::Account;
use serde_json::{json, Value};

use crate::hub::AiHub;

pub const MAX_PARENTS: usize = 40;
pub const MAX_SUBTASKS: usize = 30;
pub const MAX_CHANGES: usize = 60;
const SOURCE_MAX: usize = 300_000;
const CHANGE_KEYS: [&str; 8] = ["title", "body", "appendNote", "labels", "priority", "status", "assignee", "due"];
/// What the AI is told when it tries to write the board itself.
pub const REFUSAL: &str = "Use plan_draft: the person approves the plan first (a planner session writes nothing to the board itself; the hub writes the approved draft).";

fn clip(v: &Value, n: usize) -> String { v.as_str().map(|x| x.trim().chars().take(n).collect()).unwrap_or_default() }
fn raw_clip(v: &Value, n: usize) -> String { v.as_str().map(|x| x.chars().take(n).collect()).unwrap_or_default() }
fn labels(v: &Value) -> Vec<String> {
    let items: Vec<String> = match v { Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(), Value::String(t) => t.split(',').map(String::from).collect(), _ => vec![] };
    let mut out: Vec<String> = vec![];
    for l in items { let l: String = l.trim().chars().take(32).collect(); if !l.is_empty() && !out.contains(&l) { out.push(l); } }
    out.truncate(10); out
}
fn priority(v: &Value, dflt: i64) -> i64 { v.as_i64().or_else(|| v.as_f64().map(|x| x as i64)).unwrap_or(dflt).clamp(0, 5) }
fn is_task_id(t: &str) -> bool { t.len() > 2 && t.starts_with("T-") && t[2..].bytes().all(|b| b.is_ascii_digit()) }

/// A path under plans/ only (the planner's rule), `.md` added, no "..".
pub fn plans_path(p: &str) -> Result<String, String> {
    let p = p.trim().replace('\\', "/"); let p = p.trim_start_matches('/').to_string();
    if !p.to_lowercase().starts_with("plans/") || p.len() <= 6 || p.split('/').any(|seg| seg == ".." || seg == ".") { return Err(format!("plan_draft: note path \"{p}\" must be under plans/")); }
    Ok(if p.to_lowercase().ends_with(".md") { p } else { format!("{p}.md") })
}
/// "Planner mode: talk to the AI!" becomes "planner-mode-talk-to-the-ai".
pub fn slug(t: &str) -> String {
    let mut out = String::new();
    for c in t.to_lowercase().chars() { if c.is_ascii_alphanumeric() { out.push(c); } else if !out.is_empty() && !out.ends_with('-') { out.push('-'); } }
    let out: String = out.trim_matches('-').chars().take(40).collect::<String>().trim_matches('-').to_string();
    if out.is_empty() { "plan".into() } else { out }
}

fn subtask(raw: &Value, pn: usize, n: usize, dflt_owner: &str, dflt_pri: i64) -> Result<Value, String> {
    if !raw.is_object() { return Err(format!("plan_draft: parent {pn} sub-task {n}: must be an object {{title, body, owner, labels, priority}}")); }
    let title = clip(&raw["title"], 200); if title.is_empty() { return Err(format!("plan_draft: parent {pn} sub-task {n} needs a title")); }
    let owner = clip(&raw["owner"], 60).to_lowercase();
    Ok(json!({ "title": title, "body": raw_clip(&raw["body"], 20_000), "owner": if owner.is_empty() { dflt_owner.to_string() } else { owner }, "labels": labels(&raw["labels"]), "priority": priority(&raw["priority"], dflt_pri) }))
}
fn parent(raw: &Value, n: usize) -> Result<Value, String> {
    if !raw.is_object() { return Err(format!("plan_draft: parent {n} must be an object {{title, body, owner, labels, priority, status, blockedOn, subtasks}}")); }
    let title = clip(&raw["title"], 200); if title.is_empty() { return Err(format!("plan_draft: parent {n} needs a title")); }
    let owner = clip(&raw["owner"], 60).to_lowercase();
    if owner.is_empty() { return Err(format!("plan_draft: parent {n} (\"{title}\") has no owner: no task leaves without one (rule 4.3; give the person's username, or ask who)")); }
    let pri = priority(&raw["priority"], 2);
    let blocked_on = clip(&raw["blockedOn"], 200);
    let status = match raw["status"].as_str() { Some("backlog") => "backlog", _ => "todo" };
    let subs = raw["subtasks"].as_array().cloned().unwrap_or_default();
    if subs.len() > MAX_SUBTASKS { return Err(format!("plan_draft: parent {n} has {} sub-tasks (at most {MAX_SUBTASKS}: split it further)", subs.len())); }
    let subtasks: Result<Vec<Value>, String> = subs.iter().enumerate().map(|(i, x)| subtask(x, n, i + 1, &owner, pri)).collect();
    Ok(json!({ "title": title, "body": raw_clip(&raw["body"], 30_000), "owner": owner, "labels": labels(&raw["labels"]), "priority": pri,
        "status": if blocked_on.is_empty() { status } else { "backlog" }, "blockedOn": blocked_on, "subtasks": subtasks? }))
}
fn change(raw: &Value, n: usize, parents: usize) -> Result<Value, String> {
    let id = clip(&raw["id"], 20);
    if !is_task_id(&id) { return Err(format!("plan_draft: existing change {n}: \"{id}\" is not a task id like T-12")); }
    let mut ch = serde_json::Map::new();
    match &raw["change"] {
        Value::Object(o) => {
            if o.contains_key("delete") || o.contains_key("remove") { return Err(format!("plan_draft: existing change {n} ({id}): a planner never deletes a task; say it in notPlanned and a person does it")); }
            for k in CHANGE_KEYS { if let Some(v) = o.get(k) { ch.insert(k.into(), v.clone()); } }
            if ch.get("status").and_then(Value::as_str) == Some("done") { return Err(format!("plan_draft: existing change {n} ({id}): a planner does not complete tasks")); }
            if let Some(st) = ch.get("status").and_then(Value::as_str) { if !["backlog", "todo", "doing", "review"].contains(&st) { ch.remove("status"); } }
        }
        Value::String(t) if !t.trim().is_empty() => { ch.insert("appendNote".into(), json!(t.trim().chars().take(8000).collect::<String>())); }
        _ => return Err(format!("plan_draft: existing change {n} ({id}) needs a \"change\" (the fields to update: title, body, appendNote, labels, priority, status, assignee, due, or a note text)")),
    }
    if ch.is_empty() { return Err(format!("plan_draft: existing change {n} ({id}) changes nothing")); }
    let par = raw["parent"].as_u64().filter(|p| (*p as usize) < parents).map(|p| json!(p));
    Ok(json!({ "id": id, "change": ch, "reason": clip(&raw["reason"], 500), "parent": par.unwrap_or(Value::Null) }))
}

/// The AI's `plan_draft` arguments read into the draft the page shows and the hub writes. Err = what is wrong, for the AI to fix.
pub fn normalize(args: &Value) -> Result<Value, String> {
    let ps = args["parents"].as_array().cloned().unwrap_or_default();
    if ps.is_empty() { return Err("plan_draft: parents is empty (a draft needs at least one parent task)".into()); }
    if ps.len() > MAX_PARENTS { return Err(format!("plan_draft: {} parents (at most {MAX_PARENTS}: draft the rest in a second plan)", ps.len())); }
    let parents: Vec<Value> = ps.iter().enumerate().map(|(i, x)| parent(x, i + 1)).collect::<Result<_, _>>()?;
    let cs = args["existingChanges"].as_array().cloned().unwrap_or_default();
    if cs.len() > MAX_CHANGES { return Err(format!("plan_draft: {} existing changes (at most {MAX_CHANGES})", cs.len())); }
    let changes: Vec<Value> = cs.iter().enumerate().map(|(i, x)| change(x, i + 1, parents.len())).collect::<Result<_, _>>()?;
    let not_planned: Vec<Value> = args["notPlanned"].as_array().map(|a| a.iter().filter_map(|x| match x { Value::String(t) => Some(json!({ "text": t.chars().take(1000).collect::<String>(), "why": "" })), Value::Object(_) => Some(json!({ "text": clip(&x["text"], 1000), "why": clip(&x["why"], 500) })), _ => None }).filter(|x| !s(x, "text").is_empty()).take(100).collect()).unwrap_or_default();
    let title = { let t = clip(&args["title"], 200); if t.is_empty() { s(&parents[0], "title") } else { t } };
    let source_text = raw_clip(&args["sourceText"], SOURCE_MAX);
    let source_note = if args["sourceNote"].as_str().map(|x| !x.trim().is_empty()).unwrap_or(false) { plans_path(args["sourceNote"].as_str().unwrap_or(""))? } else if source_text.trim().is_empty() { String::new() } else { format!("plans/{}-original.md", slug(&title)) };
    Ok(json!({ "title": title, "topic": slug(&title), "parents": parents, "existingChanges": changes, "notPlanned": not_planned, "sourceText": source_text, "sourceNote": source_note }))
}

fn day() -> String { pgm_core_internal::time::iso_now().chars().take(10).collect() }
fn read_note(hub: &dyn AiHub, path: &str, who: &str) -> Option<String> {
    let r = hub.notes_tool("notes_read", &json!({ "path": path }), who);
    if r["ok"] == json!(true) { r["result"]["text"].as_str().map(String::from) } else { None }
}
fn write_note(hub: &dyn AiHub, path: &str, text: &str, who: &str) -> Result<(), String> {
    let r = hub.notes_tool("notes_write", &json!({ "path": path, "text": text }), who);
    if r["ok"] == json!(true) { Ok(()) } else { Err(s(&r, "error")) }
}
fn task_id(r: &Value) -> Option<String> { if r["ok"] == json!(true) { r["result"]["task"]["id"].as_str().map(String::from) } else { None } }
fn err_of(r: &Value) -> String { let e = r["error"].as_str().map(String::from).unwrap_or_else(|| r["error"].to_string()); if e.is_empty() || e == "null" { "refused".into() } else { e } }

fn parent_body(p: &Value, source: &str, subs: &[(String, String, String)], blocked: &str) -> String {
    let mut b = raw_clip(&p["body"], 30_000).trim().to_string();
    let mut meta = format!("**Owner:** {}", s(p, "owner"));
    if !blocked.is_empty() { meta += &format!("  \n**Blocked on:** {blocked}"); }
    if !source.is_empty() { meta += &format!("  \n**Source:** {source} (the original wording, word for word)"); }
    b = if b.is_empty() { meta } else { format!("{b}\n\n{meta}") };
    if !subs.is_empty() { b += "\n\n**Sub-tasks:**"; for (id, title, owner) in subs { b += &format!("\n- {id} · {title} ({owner})"); } }
    b.chars().take(48_000).collect()
}

/// Writes the approved part of a draft, in the order of rule 12, as "<person> · Claude" (the board store's agent actor):
/// the source note, the parents (`approve` = their indexes, 0-based), their sub-tasks, then the changes to existing tasks
/// (all of them only with `with_changes`; one tied to a parent by `parent` goes with it). Then T-281: the plan summary to
/// the chat once, the owner map appended to plans/<topic>-plan.md, and a per-owner list of the new tasks.
/// Returns what was written (task ids first); nothing here throws, each refusal is listed.
pub fn write_plan(hub: &dyn AiHub, a: &Account, draft: &Value, approve: &[usize], with_changes: bool) -> Value {
    let who = hub.name_of(&a.username); let user = a.username.as_str();
    let mut errors: Vec<Value> = vec![];
    let parents = draft["parents"].as_array().cloned().unwrap_or_default();
    // 1. the source note: word for word; appended with the date when the note exists
    let src_path = s(draft, "sourceNote"); let src_text = s(draft, "sourceText");
    let mut source_written = String::new();
    if !src_path.is_empty() && !src_text.trim().is_empty() {
        let text = match read_note(hub, &src_path, &who) { Some(old) if !old.trim().is_empty() => format!("{}\n\n---\n\n## {} · {}\n\n{}\n", old.trim_end(), day(), s(draft, "title"), src_text), _ => format!("# {}: the original wording\n\nSaved word for word on {} by {} · Claude, before the plan was written.\n\n{}\n", s(draft, "title"), day(), who, src_text) };
        match write_note(hub, &src_path, &text, &who) { Ok(()) => source_written = src_path.clone(), Err(e) => errors.push(json!({ "what": "source note", "error": e })) }
    }
    let src_ref = if source_written.is_empty() { String::new() } else { source_written.clone() };
    // 2. the parents
    struct Made { idx: usize, id: String }
    let mut made: Vec<Made> = vec![];
    let known = |o: &str| hub.account(o).is_some();
    for &i in approve {
        let Some(p) = parents.get(i) else { continue };
        let owner = s(p, "owner");
        let mut lab: Vec<String> = p["labels"].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if !s(p, "blockedOn").is_empty() && !lab.iter().any(|x| x == "blocked") { lab.push("blocked".into()); }
        let args = json!({ "title": s(p, "title"), "body": parent_body(p, &src_ref, &[], &s(p, "blockedOn")), "status": s(p, "status"), "priority": p["priority"], "labels": lab, "assignee": if known(&owner) { json!(owner) } else { Value::Null } });
        let r = hub.board_tool("create", user, &args);
        match task_id(&r) { Some(id) => made.push(Made { idx: i, id }), None => errors.push(json!({ "what": format!("parent {}", i + 1), "title": s(p, "title"), "error": err_of(&r) })) }
    }
    // 3. the sub-tasks (named T-<parent> · n/m: …, the parent id as a label), then each parent's body lists them
    let mut out_parents: Vec<Value> = vec![]; let mut for_owner: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    let mut rows: Vec<(String, String, String, i64, String, String)> = vec![]; // id, title, owner, priority, status, blocked on
    for m in &made {
        let p = &parents[m.idx]; let subs = p["subtasks"].as_array().cloned().unwrap_or_default(); let total = subs.len();
        let mut done: Vec<(String, String, String)> = vec![]; let mut out_subs: Vec<Value> = vec![];
        for (n, sdef) in subs.iter().enumerate() {
            let title = format!("{} · {}/{}: {}", m.id, n + 1, total, s(sdef, "title"));
            let mut lab = vec![m.id.clone()]; for l in sdef["labels"].as_array().cloned().unwrap_or_default() { if let Some(l) = l.as_str() { if !lab.iter().any(|x| x == l) { lab.push(l.to_string()); } } }
            let owner = s(sdef, "owner");
            let mut body = raw_clip(&sdef["body"], 20_000).trim().to_string();
            let tail = format!("**Parent:** {}{}", m.id, if src_ref.is_empty() { String::new() } else { format!("  \n**Source:** {src_ref}") });
            body = if body.is_empty() { tail } else { format!("{body}\n\n{tail}") };
            let r = hub.board_tool("create", user, &json!({ "title": title, "body": body, "status": "todo", "priority": sdef["priority"], "labels": lab, "assignee": if known(&owner) { json!(owner) } else { Value::Null } }));
            match task_id(&r) {
                Some(id) => { done.push((id.clone(), format!("{}/{}: {}", n + 1, total, s(sdef, "title")), owner.clone())); out_subs.push(json!({ "id": id, "title": title, "owner": owner }));
                    for_owner.entry(owner.clone()).or_default().push(json!({ "id": id, "title": title, "parent": m.id, "priority": sdef["priority"], "status": "todo" }));
                    rows.push((id, title.clone(), owner, sdef["priority"].as_i64().unwrap_or(2), "todo".into(), String::new())); }
                None => errors.push(json!({ "what": format!("sub-task {} of {}", n + 1, m.id), "title": title, "error": err_of(&r) })),
            }
        }
        // "Blocked on" naming another approved parent by its title or number becomes its task id
        let blocked_raw = s(p, "blockedOn");
        let blocked = if blocked_raw.is_empty() { String::new() } else {
            let low = blocked_raw.to_lowercase();
            match made.iter().find(|o| o.idx != m.idx && { let t = s(&parents[o.idx], "title").to_lowercase(); t == low || (low.len() > 3 && (t.contains(&low) || low.contains(&t))) || low == format!("parent {}", o.idx + 1) }) { Some(o) => format!("{} ({})", o.id, s(&parents[o.idx], "title")), None => blocked_raw.clone() }
        };
        let body = parent_body(p, &src_ref, &done, &blocked);
        let r = hub.board_tool("update", user, &json!({ "id": m.id, "body": body }));
        if r["ok"] != json!(true) { errors.push(json!({ "what": format!("body of {}", m.id), "error": err_of(&r) })); }
        let owner = s(p, "owner");
        for_owner.entry(owner.clone()).or_default().push(json!({ "id": m.id, "title": s(p, "title"), "parent": Value::Null, "priority": p["priority"], "status": s(p, "status") }));
        rows.push((m.id.clone(), s(p, "title"), owner.clone(), p["priority"].as_i64().unwrap_or(2), s(p, "status"), blocked.clone()));
        out_parents.push(json!({ "index": m.idx, "id": m.id, "title": s(p, "title"), "owner": owner, "blockedOn": blocked, "subtasks": out_subs }));
    }
    // 4. the changes to existing tasks (never deletes, never completes; a task someone holds is refused by the board)
    let mut out_changes: Vec<Value> = vec![];
    for c in draft["existingChanges"].as_array().cloned().unwrap_or_default() {
        let go = match c["parent"].as_u64() { Some(pi) => made.iter().any(|m| m.idx == pi as usize), None => with_changes };
        if !go { continue; }
        let id = s(&c, "id"); let mut args = c["change"].as_object().cloned().unwrap_or_default();
        if !args.contains_key("appendNote") && !s(&c, "reason").is_empty() { args.insert("appendNote".into(), json!(format!("Planned change: {}", s(&c, "reason")))); }
        args.insert("id".into(), json!(id));
        let r = hub.board_tool("update", user, &Value::Object(args));
        if r["ok"] == json!(true) { out_changes.push(json!({ "id": id, "ok": true })); } else { let e = err_of(&r); out_changes.push(json!({ "id": id, "ok": false, "error": e })); errors.push(json!({ "what": format!("change to {id}"), "error": e })); }
    }
    let created: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    // T-281: the owner map in the plan note, then the summary in the chat (once)
    let plan_path = format!("plans/{}-plan.md", s(draft, "topic")); let mut plan_note = String::new();
    if !rows.is_empty() {
        let name = |u: &str| hub.name_of(u);
        let mut sec = format!("## {} · {} (approved by {})\n\n", day(), s(draft, "title"), who);
        if !source_written.is_empty() { sec += &format!("Source note: {source_written}\n\n"); }
        sec += "| Task | Title | Owner | Priority | Status | Blocked on |\n|---|---|---|---|---|---|\n";
        for (id, title, owner, pri, st, bl) in &rows { sec += &format!("| {id} | {} | {} | {pri} | {st} | {} |\n", title.replace('|', "/"), name(owner), if bl.is_empty() { "-".to_string() } else { bl.replace('|', "/") }); }
        for c in &out_changes { if c["ok"] == json!(true) { sec += &format!("\nChanged: {}", s(c, "id")); } }
        let text = match read_note(hub, &plan_path, &who) { Some(old) if !old.trim().is_empty() => format!("{}\n\n{sec}", old.trim_end()), _ => format!("# {}: plan\n\nThe owner map, the blocked set and what each person got (T-281). Written by the hub when a plan is approved.\n\n{sec}", s(draft, "title")) };
        match write_note(hub, &plan_path, &text, &who) { Ok(()) => plan_note = plan_path.clone(), Err(e) => errors.push(json!({ "what": "plan note", "error": e })) }
    }
    let mut summary_posted = false;
    if !rows.is_empty() {
        let name = |u: &str| hub.name_of(u);
        let mut t = format!("**Plan \"{}\"** approved by {}: {} new task{} for {} owner{}.\n", s(draft, "title"), who, rows.len(), if rows.len() == 1 { "" } else { "s" }, for_owner.len(), if for_owner.len() == 1 { "" } else { "s" });
        for (owner, list) in &for_owner { let ids: Vec<String> = list.iter().map(|x| s(x, "id")).collect(); t += &format!("- {}: {}\n", name(owner), ids.join(", ")); }
        let blocked: Vec<String> = rows.iter().filter(|r| !r.5.is_empty()).map(|r| format!("{} waits for {}", r.0, r.5)).collect();
        if !blocked.is_empty() { t += &format!("Blocked: {}\n", blocked.join("; ")); }
        let mills: Vec<String> = rows.iter().filter(|r| r.2 == "mills").map(|r| r.0.clone()).collect();
        if !mills.is_empty() { t += &format!("Waits for {}: {}\n", name("mills"), mills.join(", ")); }
        if !out_changes.is_empty() { t += &format!("Existing tasks changed: {}\n", out_changes.iter().filter(|c| c["ok"] == json!(true)).map(|c| s(c, "id")).collect::<Vec<_>>().join(", ")); }
        if !source_written.is_empty() { t += &format!("Source: {source_written}"); } if !plan_note.is_empty() { t += &format!(" · Plan: {plan_note}"); }
        let r = hub.team_tool("team_say", &json!({ "text": t.trim() }), a);
        summary_posted = r["ok"] == json!(true);
        if !summary_posted { errors.push(json!({ "what": "chat summary", "error": err_of(&r) })); }
    }
    json!({ "approved": true, "created": created, "sourceNote": source_written, "planNote": plan_note, "parents": out_parents, "changes": out_changes, "summaryPosted": summary_posted, "newTasksFor": for_owner, "errors": errors })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_draft_is_checked_before_anything_is_shown() {
        assert!(normalize(&json!({})).is_err());
        assert!(normalize(&json!({ "parents": [{ "title": "x" }] })).unwrap_err().contains("no owner"));
        let d = normalize(&json!({ "title": "Planner Mode: talk!", "sourceText": "hello", "parents": [{ "title": "A", "owner": "Mills", "blockedOn": "B", "subtasks": [{ "title": "s1" }] }], "existingChanges": [{ "id": "T-5", "change": "note", "reason": "r" }] })).unwrap();
        assert_eq!(d["sourceNote"], json!("plans/planner-mode-talk-original.md"));
        assert_eq!(d["parents"][0]["owner"], json!("mills"));
        assert_eq!(d["parents"][0]["status"], json!("backlog"));
        assert_eq!(d["parents"][0]["subtasks"][0]["owner"], json!("mills"));
        assert_eq!(d["existingChanges"][0]["change"]["appendNote"], json!("note"));
        assert!(normalize(&json!({ "parents": [{ "title": "A", "owner": "m" }], "existingChanges": [{ "id": "T-5", "change": { "delete": true } }] })).is_err());
        assert!(normalize(&json!({ "parents": [{ "title": "A", "owner": "m" }], "sourceNote": "../x" })).is_err());
        assert!(plans_path("plans/a/b").unwrap().ends_with("b.md"));
    }
}
