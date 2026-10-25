//! Planner mode: draft, approve, write (T-279 / T-281; team note `plans/planner-mode.md` section 4.7, rules 7, 12, 13).
//!
//! ONE library for the VibeRoller hub (`pgm-ai-internal`: `plan.rs` / `planner.rs` are thin adapters over it) and Jcode
//! PGM (the signed-in hub's HTTP routes): the draft shape, the checks and messages, the answer body
//! (`approve: "all" | [indexes] | false`, `comment`, `revise`), the write order, the planner helpers. Everything with a
//! side effect (board, notes, chat) goes through [`write::PlanHub`]; where the two products differed, the hub's
//! behaviour is the one kept (the VibeRoller suite R300 to R309 pins it).
//!
//! - [`planner`]: the planner helpers' logic (`plan_parse`, `plan_existing`, `plan_roles`, `plan_owner`, `plan_glossary`),
//!   the roles / glossary parsing with their built-in fallbacks, and [`planner::tool`] over [`write::PlanHub`].
//! - [`draft`]: [`draft::normalize`], the AI's `plan_draft` arguments checked and read into the draft.
//! - [`view`]: the parents / sub-tasks table the person reads.
//! - [`flow`]: the answers (approve all, approve some, change with a comment, reject) as questions for the card, and the
//!   state machine that reads the replies; [`flow::Decision`] has the hub's answer body.
//! - [`write`]: [`write::write_plan`], the approved part of a draft written in the hub's order through [`write::PlanHub`].
//! - [`helpers`]: the planner helper tools (`plan_glossary` ...) in order, with what to do by hand when the hub lacks one.

pub mod draft;
pub mod flow;
pub mod helpers;
pub mod planner;
pub mod view;
pub mod write;

/// What the AI is told when it tries to write the board itself in planner mode (the hub's text, word for word).
pub const REFUSAL: &str = "Use plan_draft: the person approves the plan first (a planner session writes nothing to the board itself; the hub writes the approved draft).";

/// The board tools a planner session never calls itself (the hub refuses the same set).
pub const BOARD_WRITES: [&str; 9] =
    ["task_create", "task_update", "task_move", "task_delete", "task_claim", "task_renew", "task_release", "task_complete", "board_new"];

/// The refusal for a board write: the hub's text plus the tool name, `<REFUSAL> (<tool> refused)`.
pub fn board_refusal(tool: &str) -> String {
    format!("{REFUSAL} ({tool} refused)")
}

/// Is `tool` (a bare name or `mcp__<server>__<tool>`) a board write a planner must send through `plan_draft`?
pub fn is_board_write(tool: &str) -> bool {
    let name = tool.to_ascii_lowercase();
    let bare = name.rsplit("__").next().unwrap_or(&name);
    BOARD_WRITES.contains(&bare) || bare == "board_delete"
}

pub(crate) fn s(v: &serde_json::Value, key: &str) -> String {
    v.get(key).and_then(serde_json::Value::as_str).unwrap_or("").to_string()
}

/// Fakes for the tests of this crate: a sample draft and a hub that records what is written to it.
#[cfg(test)]
pub(crate) mod testkit {
    use crate::write::{PlanHub, NO_CHAT};
    use serde_json::{json, Value};
    use std::cell::RefCell;
    use std::collections::HashMap;

    pub(crate) fn sample() -> Value {
        crate::draft::normalize(&json!({
            "title": "Jcode planner",
            "sourceText": "words",
            "parents": [
                { "title": "Draft view in the TUI", "owner": "gee", "subtasks": [{ "title": "table" }, { "title": "keys", "owner": "sponge" }] },
                { "title": "Hub login for the writer", "owner": "mills", "blockedOn": "Draft view in the TUI", "priority": 1 },
            ],
            "existingChanges": [{ "id": "T-5", "change": { "appendNote": "n" }, "reason": "overlaps", "parent": 0 }],
            "notPlanned": [{ "text": "a poem", "why": "not a task" }],
        }))
        .unwrap()
    }

    #[derive(Default)]
    pub(crate) struct FakeHub {
        pub calls: RefCell<Vec<(String, Value)>>,
        pub notes: RefCell<HashMap<String, String>>,
        pub fail_title: RefCell<Option<String>>,
        pub chat: RefCell<bool>,
        pub said: RefCell<Vec<String>>,
        pub tasks: RefCell<Vec<Value>>,
        next: RefCell<u32>,
    }

    impl PlanHub for FakeHub {
        fn board(&self, op: &str, args: &Value) -> Value {
            if op == "list" {
                return json!({ "ok": true, "result": { "tasks": self.tasks.borrow().clone() } });
            }
            self.calls.borrow_mut().push((op.into(), args.clone()));
            if op == "create" {
                if self.fail_title.borrow().as_deref() == args["title"].as_str() {
                    return json!({ "ok": false, "error": "refused" });
                }
                *self.next.borrow_mut() += 1;
                return json!({ "ok": true, "result": { "task": { "id": format!("T-{}", self.next.borrow()) } } });
            }
            json!({ "ok": true, "result": { "task": { "id": args["id"] } } })
        }
        fn note_read(&self, path: &str) -> Option<String> {
            self.notes.borrow().get(path).cloned()
        }
        fn note_write(&self, path: &str, text: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(("note_write".into(), json!({ "path": path, "text": text })));
            self.notes.borrow_mut().insert(path.into(), text.into());
            Ok(())
        }
        fn say(&self, text: &str) -> Result<(), String> {
            if !*self.chat.borrow() {
                return Err(NO_CHAT.into());
            }
            self.said.borrow_mut().push(text.into());
            Ok(())
        }
        fn name_of(&self, user: &str) -> String {
            user.into()
        }
        fn who(&self) -> String {
            "tester".into()
        }
        fn known_user(&self, user: &str) -> bool {
            user != "stranger"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_is_the_hubs_text() {
        assert!(board_refusal("task_create").starts_with("Use plan_draft: the person approves the plan first"));
        assert!(board_refusal("task_create").ends_with("(task_create refused)"));
        for t in BOARD_WRITES {
            assert!(is_board_write(t) && is_board_write(&format!("mcp__board__{t}")), "{t}");
        }
        for t in ["task_get", "board_list", "board_calendar", "plan_draft", "ask_user", "team_say", "edit"] {
            assert!(!is_board_write(t), "{t}");
        }
    }
}
