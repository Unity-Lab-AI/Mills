//! Writes the approved part of a draft in the hub's order (rule 12): the source note, the parents (`approve` = their
//! indexes, 0-based), their sub-tasks named `T-<parent> · n/m: ...` with the parent id as a label, then the changes to
//! existing tasks (never a delete, never a completion). Then T-281: the owner map appended to `plans/<topic>-plan.md`
//! and the plan summary for the chat. This is the hub's `plan::write_plan` over a trait, so the hub's own copy and this
//! one can become one library; the trait is the only difference: [`PlanHub`] is the hub's board, notes and chat.
//! Nothing here throws: each refusal is listed in `errors`.

use crate::s;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The board, the notes and the chat of the hub the plan is written to. Board answers have the hub's shape:
/// `{ok: true, result: {task: {id}}}` or `{ok: false, error}`.
pub trait PlanHub {
    /// A board operation: `create` or `update` (the hub's board tool arguments).
    fn board(&self, op: &str, args: &Value) -> Value;
    /// The text of a note, `None` when there is none.
    fn note_read(&self, path: &str) -> Option<String>;
    fn note_write(&self, path: &str, text: &str) -> Result<(), String>;
    /// Post the plan summary in the team chat. `Err` with [`NO_CHAT`] when this hub connection has no way to.
    fn say(&self, text: &str) -> Result<(), String>;
    /// The name shown for a username.
    fn name_of(&self, user: &str) -> String;
    /// The person writing, as shown ("<who> · Claude" is the actor of every write): the hub gives the display name.
    fn who(&self) -> String;
    /// Is `user` an account of this hub? An owner who is not one is written unassigned (the board refuses a stranger).
    fn known_user(&self, user: &str) -> bool;
    /// How the notes say who wrote the plan: the hub says " by the hub", a client of a hub says nothing.
    fn written_by(&self) -> &str {
        ""
    }
}

/// What [`PlanHub::say`] answers when the chat cannot be reached from here: not an error, the summary is returned instead.
pub const NO_CHAT: &str = "no chat";

fn raw_clip(v: &Value, n: usize) -> String {
    v.as_str().map(|x| x.chars().take(n).collect()).unwrap_or_default()
}
fn task_id(r: &Value) -> Option<String> {
    if r["ok"] == json!(true) {
        r["result"]["task"]["id"].as_str().map(String::from)
    } else {
        None
    }
}
fn err_of(r: &Value) -> String {
    let e = r["error"].as_str().map(String::from).unwrap_or_else(|| r["error"].to_string());
    if e.is_empty() || e == "null" {
        "refused".into()
    } else {
        e
    }
}

fn parent_body(p: &Value, source: &str, subs: &[(String, String, String)], blocked: &str) -> String {
    let mut b = raw_clip(&p["body"], 30_000).trim().to_string();
    let mut meta = format!("**Owner:** {}", s(p, "owner"));
    if !blocked.is_empty() {
        meta += &format!("  \n**Blocked on:** {blocked}");
    }
    if !source.is_empty() {
        meta += &format!("  \n**Source:** {source} (the original wording, word for word)");
    }
    b = if b.is_empty() { meta } else { format!("{b}\n\n{meta}") };
    if !subs.is_empty() {
        b += "\n\n**Sub-tasks:**";
        for (id, title, owner) in subs {
            b += &format!("\n- {id} · {title} ({owner})");
        }
    }
    b.chars().take(48_000).collect()
}

type Row = (String, String, String, i64, String, String); // id, title, owner, priority, status, blocked on

/// Write the approved parents of `draft` (and their sub-tasks and tied changes); the untied changes only with
/// `with_changes`. `day` is today as `YYYY-MM-DD`. The result lists what was written (`created` task ids first).
pub fn write_plan(hub: &dyn PlanHub, draft: &Value, approve: &[usize], with_changes: bool, day: &str) -> Value {
    let who = hub.who();
    let mut errors: Vec<Value> = vec![];
    let parents = draft["parents"].as_array().cloned().unwrap_or_default();
    // 1. the source note: word for word; appended with the date when the note exists
    let src_path = s(draft, "sourceNote");
    let src_text = s(draft, "sourceText");
    let mut source_written = String::new();
    if !src_path.is_empty() && !src_text.trim().is_empty() {
        let text = match hub.note_read(&src_path) {
            Some(old) if !old.trim().is_empty() => format!("{}\n\n---\n\n## {day} · {}\n\n{src_text}\n", old.trim_end(), s(draft, "title")),
            _ => format!("# {}: the original wording\n\nSaved word for word on {day} by {who} · Claude, before the plan was written.\n\n{src_text}\n", s(draft, "title")),
        };
        match hub.note_write(&src_path, &text) {
            Ok(()) => source_written = src_path.clone(),
            Err(e) => errors.push(json!({ "what": "source note", "error": e })),
        }
    }
    // 2. the parents
    struct Made {
        idx: usize,
        id: String,
    }
    let mut made: Vec<Made> = vec![];
    for &i in approve {
        let Some(p) = parents.get(i) else { continue };
        let owner = s(p, "owner");
        let mut lab: Vec<String> = p["labels"].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if !s(p, "blockedOn").is_empty() && !lab.iter().any(|x| x == "blocked") {
            lab.push("blocked".into());
        }
        let args = json!({ "title": s(p, "title"), "body": parent_body(p, &source_written, &[], &s(p, "blockedOn")), "status": s(p, "status"), "priority": p["priority"], "labels": lab, "assignee": if hub.known_user(&owner) { json!(owner) } else { Value::Null } });
        let r = hub.board("create", &args);
        match task_id(&r) {
            Some(id) => made.push(Made { idx: i, id }),
            None => errors.push(json!({ "what": format!("parent {}", i + 1), "title": s(p, "title"), "error": err_of(&r) })),
        }
    }
    // 3. the sub-tasks, then each parent's body lists them
    let mut out_parents: Vec<Value> = vec![];
    let mut for_owner: BTreeMap<String, Vec<Value>> = Default::default();
    let mut rows: Vec<Row> = vec![];
    for m in &made {
        let p = &parents[m.idx];
        let subs = p["subtasks"].as_array().cloned().unwrap_or_default();
        let total = subs.len();
        let mut done: Vec<(String, String, String)> = vec![];
        let mut out_subs: Vec<Value> = vec![];
        for (n, sdef) in subs.iter().enumerate() {
            let title = format!("{} · {}/{}: {}", m.id, n + 1, total, s(sdef, "title"));
            let mut lab = vec![m.id.clone()];
            for l in sdef["labels"].as_array().cloned().unwrap_or_default() {
                if let Some(l) = l.as_str() {
                    if !lab.iter().any(|x| x == l) {
                        lab.push(l.to_string());
                    }
                }
            }
            let owner = s(sdef, "owner");
            let mut body = raw_clip(&sdef["body"], 20_000).trim().to_string();
            let tail = format!("**Parent:** {}{}", m.id, if source_written.is_empty() { String::new() } else { format!("  \n**Source:** {source_written}") });
            body = if body.is_empty() { tail } else { format!("{body}\n\n{tail}") };
            let r = hub.board("create", &json!({ "title": title, "body": body, "status": "todo", "priority": sdef["priority"], "labels": lab, "assignee": if hub.known_user(&owner) { json!(owner) } else { Value::Null } }));
            match task_id(&r) {
                Some(id) => {
                    done.push((id.clone(), format!("{}/{}: {}", n + 1, total, s(sdef, "title")), owner.clone()));
                    out_subs.push(json!({ "id": id, "title": title, "owner": owner }));
                    for_owner.entry(owner.clone()).or_default().push(json!({ "id": id, "title": title, "parent": m.id, "priority": sdef["priority"], "status": "todo" }));
                    rows.push((id, title.clone(), owner, sdef["priority"].as_i64().unwrap_or(2), "todo".into(), String::new()));
                }
                None => errors.push(json!({ "what": format!("sub-task {} of {}", n + 1, m.id), "title": title, "error": err_of(&r) })),
            }
        }
        // "Blocked on" naming another approved parent by its title or number becomes its task id
        let blocked_raw = s(p, "blockedOn");
        let blocked = if blocked_raw.is_empty() {
            String::new()
        } else {
            let low = blocked_raw.to_lowercase();
            match made.iter().find(|o| {
                o.idx != m.idx && {
                    let t = s(&parents[o.idx], "title").to_lowercase();
                    t == low || (low.len() > 3 && (t.contains(&low) || low.contains(&t))) || low == format!("parent {}", o.idx + 1)
                }
            }) {
                Some(o) => format!("{} ({})", o.id, s(&parents[o.idx], "title")),
                None => blocked_raw.clone(),
            }
        };
        let r = hub.board("update", &json!({ "id": m.id, "body": parent_body(p, &source_written, &done, &blocked) }));
        if r["ok"] != json!(true) {
            errors.push(json!({ "what": format!("body of {}", m.id), "error": err_of(&r) }));
        }
        let owner = s(p, "owner");
        for_owner.entry(owner.clone()).or_default().push(json!({ "id": m.id, "title": s(p, "title"), "parent": Value::Null, "priority": p["priority"], "status": s(p, "status") }));
        rows.push((m.id.clone(), s(p, "title"), owner.clone(), p["priority"].as_i64().unwrap_or(2), s(p, "status"), blocked.clone()));
        out_parents.push(json!({ "index": m.idx, "id": m.id, "title": s(p, "title"), "owner": owner, "blockedOn": blocked, "subtasks": out_subs }));
    }
    // 4. the changes to existing tasks (never deletes, never completes; a task someone holds is refused by the board)
    let mut out_changes: Vec<Value> = vec![];
    for c in draft["existingChanges"].as_array().cloned().unwrap_or_default() {
        let go = match c["parent"].as_u64() {
            Some(pi) => made.iter().any(|m| m.idx == pi as usize),
            None => with_changes,
        };
        if !go {
            continue;
        }
        let id = s(&c, "id");
        let mut args = c["change"].as_object().cloned().unwrap_or_default();
        if !args.contains_key("appendNote") && !s(&c, "reason").is_empty() {
            args.insert("appendNote".into(), json!(format!("Planned change: {}", s(&c, "reason"))));
        }
        args.insert("id".into(), json!(id));
        let r = hub.board("update", &Value::Object(args));
        if r["ok"] == json!(true) {
            out_changes.push(json!({ "id": id, "ok": true }));
        } else {
            let e = err_of(&r);
            out_changes.push(json!({ "id": id, "ok": false, "error": e }));
            errors.push(json!({ "what": format!("change to {id}"), "error": e }));
        }
    }
    let created: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    // T-281: the owner map in the plan note, then the summary (once)
    let plan_path = format!("plans/{}-plan.md", s(draft, "topic"));
    let mut plan_note = String::new();
    if !rows.is_empty() {
        let mut sec = format!("## {day} · {} (approved by {who})\n\n", s(draft, "title"));
        if !source_written.is_empty() {
            sec += &format!("Source note: {source_written}\n\n");
        }
        sec += "| Task | Title | Owner | Priority | Status | Blocked on |\n|---|---|---|---|---|---|\n";
        for (id, title, owner, pri, st, bl) in &rows {
            sec += &format!("| {id} | {} | {} | {pri} | {st} | {} |\n", title.replace('|', "/"), hub.name_of(owner), if bl.is_empty() { "-".to_string() } else { bl.replace('|', "/") });
        }
        for c in &out_changes {
            if c["ok"] == json!(true) {
                sec += &format!("\nChanged: {}", s(c, "id"));
            }
        }
        let text = match hub.note_read(&plan_path) {
            Some(old) if !old.trim().is_empty() => format!("{}\n\n{sec}", old.trim_end()),
            _ => format!("# {}: plan\n\nThe owner map, the blocked set and what each person got (T-281). Written{} when a plan is approved.\n\n{sec}", s(draft, "title"), hub.written_by()),
        };
        match hub.note_write(&plan_path, &text) {
            Ok(()) => plan_note = plan_path.clone(),
            Err(e) => errors.push(json!({ "what": "plan note", "error": e })),
        }
    }
    let mut summary = String::new();
    let mut summary_posted = false;
    if !rows.is_empty() {
        let name = |u: &str| hub.name_of(u);
        let mut t = format!("**Plan \"{}\"** approved by {who}: {} new task{} for {} owner{}.\n", s(draft, "title"), rows.len(), if rows.len() == 1 { "" } else { "s" }, for_owner.len(), if for_owner.len() == 1 { "" } else { "s" });
        for (owner, list) in &for_owner {
            let ids: Vec<String> = list.iter().map(|x| s(x, "id")).collect();
            t += &format!("- {}: {}\n", name(owner), ids.join(", "));
        }
        let blocked: Vec<String> = rows.iter().filter(|r| !r.5.is_empty()).map(|r| format!("{} waits for {}", r.0, r.5)).collect();
        if !blocked.is_empty() {
            t += &format!("Blocked: {}\n", blocked.join("; "));
        }
        let mills: Vec<String> = rows.iter().filter(|r| r.2 == "mills").map(|r| r.0.clone()).collect();
        if !mills.is_empty() {
            t += &format!("Waits for {}: {}\n", name("mills"), mills.join(", "));
        }
        if !out_changes.is_empty() {
            t += &format!("Existing tasks changed: {}\n", out_changes.iter().filter(|c| c["ok"] == json!(true)).map(|c| s(c, "id")).collect::<Vec<_>>().join(", "));
        }
        if !source_written.is_empty() {
            t += &format!("Source: {source_written}");
        }
        if !plan_note.is_empty() {
            t += &format!(" · Plan: {plan_note}");
        }
        summary = t.trim().to_string();
        match hub.say(&summary) {
            Ok(()) => summary_posted = true,
            Err(e) if e == NO_CHAT => {}
            Err(e) => errors.push(json!({ "what": "chat summary", "error": e })),
        }
    }
    json!({ "approved": true, "created": created, "sourceNote": source_written, "planNote": plan_note, "parents": out_parents, "changes": out_changes, "summaryPosted": summary_posted, "summary": summary, "newTasksFor": for_owner, "errors": errors })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{sample, FakeHub};

    #[test]
    fn the_write_order_is_note_parents_sub_tasks_changes() {
        let hub = FakeHub::default();
        let r = write_plan(&hub, &sample(), &[0, 1], true, "2026-10-25");
        let calls = hub.calls.borrow();
        let order: Vec<String> = calls.iter().map(|c| format!("{}:{}", c.0, c.1["title"].as_str().or(c.1["id"].as_str()).or(c.1["path"].as_str()).unwrap_or(""))).collect();
        assert_eq!(order[0], "note_write:plans/jcode-planner-original.md", "{order:?}");
        assert_eq!(order[1], "create:Draft view in the TUI");
        assert_eq!(order[2], "create:Hub login for the writer", "all parents before any sub-task");
        assert_eq!(order[3], "create:T-1 · 1/2: table");
        assert_eq!(order[4], "create:T-1 · 2/2: keys");
        assert!(order[5].starts_with("update:T-1") && order[6].starts_with("update:T-2"), "{order:?}");
        let change = order.iter().position(|o| o == "update:T-5").expect("the change");
        assert!(change > 6, "changes last: {order:?}");
        assert!(order.last().unwrap().starts_with("note_write:plans/jcode-planner-plan.md"), "{order:?}");
        assert_eq!(r["created"], json!(["T-3", "T-4", "T-1", "T-2"]), "the hub lists each parent's sub-tasks, then the parent");
        assert_eq!(r["summaryPosted"], false);
        assert!(r["summary"].as_str().unwrap().contains("4 new tasks for 3 owners"), "{}", r["summary"]);
    }

    #[test]
    fn a_planner_never_deletes_or_completes() {
        let hub = FakeHub::default();
        write_plan(&hub, &sample(), &[0, 1], true, "d");
        for c in hub.calls.borrow().iter() {
            assert!(matches!(c.0.as_str(), "create" | "update" | "note_write" | "note_read"), "{}", c.0);
            assert_ne!(c.1["status"], json!("done"));
        }
    }

    #[test]
    fn titles_labels_blocked_on_and_the_source_are_the_hubs() {
        let hub = FakeHub::default();
        write_plan(&hub, &sample(), &[0, 1], true, "d");
        let calls = hub.calls.borrow();
        let second = calls.iter().filter(|c| c.0 == "create").nth(1).unwrap();
        assert_eq!(second.1["status"], "backlog");
        assert!(second.1["labels"].as_array().unwrap().iter().any(|l| l == "blocked"));
        assert!(second.1["body"].as_str().unwrap().contains("**Source:** plans/jcode-planner-original.md"));
        let sub = calls.iter().find(|c| c.0 == "create" && c.1["title"] == "T-1 · 2/2: keys").unwrap();
        assert_eq!(sub.1["labels"], json!(["T-1"]));
        assert_eq!(sub.1["assignee"], "sponge");
        let body_update = calls.iter().find(|c| c.0 == "update" && c.1["id"] == "T-2").unwrap();
        assert!(body_update.1["body"].as_str().unwrap().contains("**Blocked on:** T-1 (Draft view in the TUI)"), "{}", body_update.1["body"]);
        let parent_update = calls.iter().find(|c| c.0 == "update" && c.1["id"] == "T-1").unwrap();
        assert!(parent_update.1["body"].as_str().unwrap().contains("- T-3 · 1/2: table (gee)"));
    }

    #[test]
    fn a_refused_write_is_listed_and_the_rest_goes_on() {
        let hub = FakeHub::default();
        *hub.fail_title.borrow_mut() = Some("Draft view in the TUI".into());
        let r = write_plan(&hub, &sample(), &[0, 1], true, "d");
        assert_eq!(r["errors"][0]["what"], "parent 1");
        assert_eq!(r["created"].as_array().unwrap().len(), 1, "parent 2 still written");
        // a change tied to the failed parent is not applied
        assert!(r["changes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn an_existing_source_note_is_appended_with_the_date() {
        let hub = FakeHub::default();
        hub.notes.borrow_mut().insert("plans/jcode-planner-original.md".into(), "# old\n\nbefore".into());
        write_plan(&hub, &sample(), &[0], false, "2026-10-25");
        let text = hub.notes.borrow()["plans/jcode-planner-original.md"].clone();
        assert!(text.starts_with("# old\n\nbefore\n\n---\n\n## 2026-10-25 · Jcode planner\n\nwords"), "{text}");
    }

    #[test]
    fn a_stranger_owner_is_written_unassigned_and_the_notes_say_who_wrote() {
        let hub = FakeHub::default();
        let mut d = sample();
        d["parents"][1]["owner"] = json!("stranger");
        write_plan(&hub, &d, &[1], false, "2026-10-25");
        let calls = hub.calls.borrow();
        let c = calls.iter().find(|c| c.0 == "create").unwrap();
        assert!(c.1["assignee"].is_null());
        let src = hub.notes.borrow()["plans/jcode-planner-original.md"].clone();
        assert!(src.contains("by tester · Claude, before the plan was written"), "{src}");
    }

    #[test]
    fn the_chat_summary_is_posted_when_the_hub_can() {
        let hub = FakeHub::default();
        *hub.chat.borrow_mut() = true;
        let r = write_plan(&hub, &sample(), &[1], false, "d");
        assert_eq!(r["summaryPosted"], true);
        assert!(hub.said.borrow()[0].contains("**Plan \"Jcode planner\"** approved by tester"));
    }
}
