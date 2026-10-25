//! The draft as the person reads it: a parents / sub-tasks table in plain, aligned text (it is shown in the terminal's
//! question card), then the changes to existing tasks, what is not planned, and where the source is kept.

use crate::s;
use serde_json::Value;

const TITLE_W: usize = 46;

fn cut(t: &str, n: usize) -> String {
    let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= n {
        t
    } else {
        format!("{}…", t.chars().take(n - 1).collect::<String>())
    }
}

fn pad(t: &str, n: usize) -> String {
    let len = t.chars().count();
    format!("{t}{}", " ".repeat(n.saturating_sub(len)))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `(parents, sub-tasks, changes)` of a draft.
pub fn counts(draft: &Value) -> (usize, usize, usize) {
    let parents = draft["parents"].as_array().cloned().unwrap_or_default();
    let subs = parents.iter().map(|p| p["subtasks"].as_array().map(|a| a.len()).unwrap_or(0)).sum();
    (parents.len(), subs, draft["existingChanges"].as_array().map(|a| a.len()).unwrap_or(0))
}

/// The whole draft as text. Parent numbers start at 1 (the card's checklist uses the same numbers).
pub fn table(draft: &Value) -> String {
    let (np, ns, nc) = counts(draft);
    let mut out = format!("Plan \"{}\": {}, {}", s(draft, "title"), plural(np, "parent task", "parent tasks"), plural(ns, "sub-task", "sub-tasks"));
    if nc > 0 {
        out += &format!(", {} to existing tasks", plural(nc, "change", "changes"));
    }
    out += "\n\n";
    out += &format!("{} {} {} {} {} {}\n", pad("#", 5), pad("Task", TITLE_W), pad("Owner", 8), pad("Pri", 3), pad("Status", 8), "Blocked on");
    for (i, p) in draft["parents"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        let blocked = s(p, "blockedOn");
        out += &format!(
            "{} {} {} {} {} {}\n",
            pad(&(i + 1).to_string(), 5),
            pad(&cut(&s(p, "title"), TITLE_W), TITLE_W),
            pad(&cut(&s(p, "owner"), 8), 8),
            pad(&p["priority"].as_i64().unwrap_or(2).to_string(), 3),
            pad(&s(p, "status"), 8),
            if blocked.is_empty() { "-".to_string() } else { cut(&blocked, 30) },
        );
        let subs = p["subtasks"].as_array().cloned().unwrap_or_default();
        let total = subs.len();
        for (n, sub) in subs.iter().enumerate() {
            out += &format!(
                "{} {} {} {}\n",
                pad(&format!("  {}/{}", n + 1, total), 5),
                pad(&cut(&s(sub, "title"), TITLE_W), TITLE_W),
                pad(&cut(&s(sub, "owner"), 8), 8),
                sub["priority"].as_i64().unwrap_or(2),
            );
        }
    }
    let changes = draft["existingChanges"].as_array().cloned().unwrap_or_default();
    if !changes.is_empty() {
        out += "\nChanges to existing tasks (never a delete or a completion):\n";
        for c in &changes {
            let fields: Vec<String> = c["change"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
            let with = c["parent"].as_u64().map(|p| format!(" (with parent {})", p + 1)).unwrap_or_default();
            let why = s(c, "reason");
            out += &format!("  {}: {}{}{}\n", s(c, "id"), fields.join(", "), with, if why.is_empty() { String::new() } else { format!(" - {}", cut(&why, 80)) });
        }
    }
    let np_list = draft["notPlanned"].as_array().cloned().unwrap_or_default();
    if !np_list.is_empty() {
        out += "\nNot planned:\n";
        for x in &np_list {
            let why = s(x, "why");
            out += &format!("  {}{}\n", cut(&s(x, "text"), 80), if why.is_empty() { String::new() } else { format!(" - {}", cut(&why, 80)) });
        }
    }
    let note = s(draft, "sourceNote");
    if !note.is_empty() {
        out += &format!("\nYour words are kept word for word in {note}.\n");
    }
    out += "Nothing is written to the board until you approve.";
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft::normalize;
    use crate::testkit::sample;
    use serde_json::json;

    #[test]
    fn the_table_lists_parents_then_their_sub_tasks_then_the_rest() {
        let t = table(&sample());
        assert!(t.starts_with("Plan \"Jcode planner\": 2 parent tasks, 2 sub-tasks, 1 change to existing tasks"), "{t}");
        let lines: Vec<&str> = t.lines().collect();
        let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} in {t}"));
        assert!(row("Draft view in the TUI") < row("1/2") && row("1/2") < row("2/2") && row("2/2") < row("Hub login for the writer"));
        assert!(t.contains("sponge") && t.contains("backlog"), "{t}");
        assert!(t.contains("T-5: appendNote (with parent 1) - overlaps"), "{t}");
        assert!(t.contains("a poem - not a task") && t.contains("plans/jcode-planner-original.md"), "{t}");
        assert!(t.ends_with("Nothing is written to the board until you approve."));
        assert_eq!(counts(&sample()), (2, 2, 1));
    }

    #[test]
    fn long_titles_are_cut_not_wrapped() {
        let d = normalize(&json!({ "parents": [{ "title": "x".repeat(150), "owner": "gee" }] })).unwrap();
        assert!(table(&d).lines().nth(3).unwrap().contains('…'));
    }
}
