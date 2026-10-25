//! The draft check: the AI's `plan_draft` arguments read into the draft the person sees and the writer writes. The rules
//! and the error texts are the hub's (`plan.rs` `normalize`), so a draft means the same in both products.

use crate::s;
use serde_json::{json, Map, Value};

pub const MAX_PARENTS: usize = 40;
pub const MAX_SUBTASKS: usize = 30;
pub const MAX_CHANGES: usize = 60;
const SOURCE_MAX: usize = 300_000;
const CHANGE_KEYS: [&str; 8] = ["title", "body", "appendNote", "labels", "priority", "status", "assignee", "due"];

fn clip(v: &Value, n: usize) -> String {
    v.as_str().map(|x| x.trim().chars().take(n).collect()).unwrap_or_default()
}
fn raw_clip(v: &Value, n: usize) -> String {
    v.as_str().map(|x| x.chars().take(n).collect()).unwrap_or_default()
}
fn labels(v: &Value) -> Vec<String> {
    let items: Vec<String> = match v {
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Value::String(t) => t.split(',').map(String::from).collect(),
        _ => vec![],
    };
    let mut out: Vec<String> = vec![];
    for l in items {
        let l: String = l.trim().chars().take(32).collect();
        if !l.is_empty() && !out.contains(&l) {
            out.push(l);
        }
    }
    out.truncate(10);
    out
}
fn priority(v: &Value, dflt: i64) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|x| x as i64)).unwrap_or(dflt).clamp(0, 5)
}
fn is_task_id(t: &str) -> bool {
    t.len() > 2 && t.starts_with("T-") && t[2..].bytes().all(|b| b.is_ascii_digit())
}

/// A path under plans/ only (the planner's rule), `.md` added, no "..".
pub fn plans_path(p: &str) -> Result<String, String> {
    let p = p.trim().replace('\\', "/");
    let p = p.trim_start_matches('/').to_string();
    if !p.to_lowercase().starts_with("plans/") || p.len() <= 6 || p.split('/').any(|seg| seg == ".." || seg == ".") {
        return Err(format!("plan_draft: note path \"{p}\" must be under plans/"));
    }
    Ok(if p.to_lowercase().ends_with(".md") { p } else { format!("{p}.md") })
}

/// "Planner mode: talk to the AI!" becomes "planner-mode-talk-to-the-ai".
pub fn slug(t: &str) -> String {
    let mut out = String::new();
    for c in t.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(40).collect::<String>().trim_matches('-').to_string();
    if out.is_empty() {
        "plan".into()
    } else {
        out
    }
}

fn subtask(raw: &Value, pn: usize, n: usize, dflt_owner: &str, dflt_pri: i64) -> Result<Value, String> {
    if !raw.is_object() {
        return Err(format!("plan_draft: parent {pn} sub-task {n}: must be an object {{title, body, owner, labels, priority}}"));
    }
    let title = clip(&raw["title"], 200);
    if title.is_empty() {
        return Err(format!("plan_draft: parent {pn} sub-task {n} needs a title"));
    }
    let owner = clip(&raw["owner"], 60).to_lowercase();
    Ok(json!({
        "title": title, "body": raw_clip(&raw["body"], 20_000),
        "owner": if owner.is_empty() { dflt_owner.to_string() } else { owner },
        "labels": labels(&raw["labels"]), "priority": priority(&raw["priority"], dflt_pri),
    }))
}

fn parent(raw: &Value, n: usize) -> Result<Value, String> {
    if !raw.is_object() {
        return Err(format!("plan_draft: parent {n} must be an object {{title, body, owner, labels, priority, status, blockedOn, subtasks}}"));
    }
    let title = clip(&raw["title"], 200);
    if title.is_empty() {
        return Err(format!("plan_draft: parent {n} needs a title"));
    }
    let owner = clip(&raw["owner"], 60).to_lowercase();
    if owner.is_empty() {
        return Err(format!("plan_draft: parent {n} (\"{title}\") has no owner: no task leaves without one (rule 4.3; give the person's username, or ask who)"));
    }
    let pri = priority(&raw["priority"], 2);
    let blocked_on = clip(&raw["blockedOn"], 200);
    let status = match raw["status"].as_str() {
        Some("backlog") => "backlog",
        _ => "todo",
    };
    let subs = raw["subtasks"].as_array().cloned().unwrap_or_default();
    if subs.len() > MAX_SUBTASKS {
        return Err(format!("plan_draft: parent {n} has {} sub-tasks (at most {MAX_SUBTASKS}: split it further)", subs.len()));
    }
    let subtasks: Result<Vec<Value>, String> = subs.iter().enumerate().map(|(i, x)| subtask(x, n, i + 1, &owner, pri)).collect();
    Ok(json!({
        "title": title, "body": raw_clip(&raw["body"], 30_000), "owner": owner, "labels": labels(&raw["labels"]), "priority": pri,
        "status": if blocked_on.is_empty() { status } else { "backlog" }, "blockedOn": blocked_on, "subtasks": subtasks?,
    }))
}

fn change(raw: &Value, n: usize, parents: usize) -> Result<Value, String> {
    let id = clip(&raw["id"], 20);
    if !is_task_id(&id) {
        return Err(format!("plan_draft: existing change {n}: \"{id}\" is not a task id like T-12"));
    }
    let mut ch = Map::new();
    match &raw["change"] {
        Value::Object(o) => {
            if o.contains_key("delete") || o.contains_key("remove") {
                return Err(format!("plan_draft: existing change {n} ({id}): a planner never deletes a task; say it in notPlanned and a person does it"));
            }
            for k in CHANGE_KEYS {
                if let Some(v) = o.get(k) {
                    ch.insert(k.into(), v.clone());
                }
            }
            if ch.get("status").and_then(Value::as_str) == Some("done") {
                return Err(format!("plan_draft: existing change {n} ({id}): a planner does not complete tasks"));
            }
            if let Some(st) = ch.get("status").and_then(Value::as_str) {
                if !["backlog", "todo", "doing", "review"].contains(&st) {
                    ch.remove("status");
                }
            }
        }
        Value::String(t) if !t.trim().is_empty() => {
            ch.insert("appendNote".into(), json!(t.trim().chars().take(8000).collect::<String>()));
        }
        _ => {
            return Err(format!("plan_draft: existing change {n} ({id}) needs a \"change\" (the fields to update: title, body, appendNote, labels, priority, status, assignee, due, or a note text)"))
        }
    }
    if ch.is_empty() {
        return Err(format!("plan_draft: existing change {n} ({id}) changes nothing"));
    }
    let par = raw["parent"].as_u64().filter(|p| (*p as usize) < parents).map(|p| json!(p));
    Ok(json!({ "id": id, "change": ch, "reason": clip(&raw["reason"], 500), "parent": par.unwrap_or(Value::Null) }))
}

/// The AI's `plan_draft` arguments read into the draft. `Err` is what is wrong, for the AI to fix.
pub fn normalize(args: &Value) -> Result<Value, String> {
    let ps = args["parents"].as_array().cloned().unwrap_or_default();
    if ps.is_empty() {
        return Err("plan_draft: parents is empty (a draft needs at least one parent task)".into());
    }
    if ps.len() > MAX_PARENTS {
        return Err(format!("plan_draft: {} parents (at most {MAX_PARENTS}: draft the rest in a second plan)", ps.len()));
    }
    let parents: Vec<Value> = ps.iter().enumerate().map(|(i, x)| parent(x, i + 1)).collect::<Result<_, _>>()?;
    let cs = args["existingChanges"].as_array().cloned().unwrap_or_default();
    if cs.len() > MAX_CHANGES {
        return Err(format!("plan_draft: {} existing changes (at most {MAX_CHANGES})", cs.len()));
    }
    let changes: Vec<Value> = cs.iter().enumerate().map(|(i, x)| change(x, i + 1, parents.len())).collect::<Result<_, _>>()?;
    let not_planned: Vec<Value> = args["notPlanned"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| match x {
                    Value::String(t) => Some(json!({ "text": t.chars().take(1000).collect::<String>(), "why": "" })),
                    Value::Object(_) => Some(json!({ "text": clip(&x["text"], 1000), "why": clip(&x["why"], 500) })),
                    _ => None,
                })
                .filter(|x| !s(x, "text").is_empty())
                .take(100)
                .collect()
        })
        .unwrap_or_default();
    let title = {
        let t = clip(&args["title"], 200);
        if t.is_empty() {
            s(&parents[0], "title")
        } else {
            t
        }
    };
    let source_text = raw_clip(&args["sourceText"], SOURCE_MAX);
    let source_note = if args["sourceNote"].as_str().map(|x| !x.trim().is_empty()).unwrap_or(false) {
        plans_path(args["sourceNote"].as_str().unwrap_or(""))?
    } else if source_text.trim().is_empty() {
        String::new()
    } else {
        format!("plans/{}-original.md", slug(&title))
    };
    Ok(json!({
        "title": title, "topic": slug(&title), "parents": parents, "existingChanges": changes,
        "notPlanned": not_planned, "sourceText": source_text, "sourceNote": source_note,
    }))
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
        assert!(normalize(&json!({ "parents": [{ "title": "A", "owner": "m" }], "existingChanges": [{ "id": "T-5", "change": { "status": "done" } }] })).unwrap_err().contains("does not complete"));
        assert!(normalize(&json!({ "parents": [{ "title": "A", "owner": "m" }], "sourceNote": "../x" })).is_err());
        assert!(plans_path("plans/a/b").unwrap().ends_with("b.md"));
    }
}
