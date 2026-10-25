//! Planner mode: draft, approve, write (T-279 / T-281; team note plans/planner-mode.md § 4.7, 5 rules 7, 12, 13 and § 6).
//! The planner AI never writes to the board. It sends the whole plan as a draft (the team MCP tool `plan_draft`); the
//! person approves all, some or none on their page; and only then does the HUB write it, in the order of rule 12: the
//! source note, the parents, the sub-tasks, the changes to existing tasks. Then the summary goes to the chat once and the
//! owner map is appended to the plan note (T-281).
//!
//! The logic is the library `pgm-plan-internal` (T-301: ONE crate for this hub and Jcode PGM; mirrored from pgm-libs).
//! This file is only the hub's side of its [`PlanHub`] trait: the board, the notes and the chat over the hub's own
//! stores ([`HubPlan`]), and the day for the notes. The open drafts, their events and the waiting session are in work.rs
//! (`Work::plan_draft`, `Work::plan_answer`).
use pgm_identity_internal::Account;
use pgm_plan_internal::write::PlanHub;
use serde_json::{json, Value};

use crate::hub::AiHub;

pub use pgm_plan_internal::draft::{normalize, plans_path, slug, MAX_CHANGES, MAX_PARENTS, MAX_SUBTASKS};
pub use pgm_plan_internal::flow::{approve, change_result, reject_result, Decision};
pub use pgm_plan_internal::REFUSAL;

/// The hub's stores for one person, as the library's [`PlanHub`]: every write is theirs as "<person> · Claude" (the
/// board store's agent actor), the notes are saved as that name, the summary is posted as that person's agent.
pub struct HubPlan<'a> { hub: &'a dyn AiHub, user: String, who: String }

impl<'a> HubPlan<'a> {
    pub fn new(hub: &'a dyn AiHub, user: &str) -> Self { HubPlan { hub, user: user.to_string(), who: hub.name_of(user) } }
}

fn err_of(r: &Value) -> String {
    let e = r["error"].as_str().map(String::from).unwrap_or_else(|| r["error"].to_string());
    if e.is_empty() || e == "null" { "refused".into() } else { e }
}

impl PlanHub for HubPlan<'_> {
    fn board(&self, op: &str, args: &Value) -> Value { self.hub.board_tool(op, &self.user, args) }
    fn note_read(&self, path: &str) -> Option<String> {
        let r = self.hub.notes_tool("notes_read", &json!({ "path": path }), &self.who);
        if r["ok"] == json!(true) { r["result"]["text"].as_str().map(String::from) } else { None }
    }
    fn note_write(&self, path: &str, text: &str) -> Result<(), String> {
        let r = self.hub.notes_tool("notes_write", &json!({ "path": path, "text": text }), &self.who);
        if r["ok"] == json!(true) { Ok(()) } else { Err(err_of(&r)) }
    }
    fn say(&self, text: &str) -> Result<(), String> {
        let Some(a) = self.hub.account(&self.user) else { return Err("unknown account".into()) };
        let r = self.hub.team_tool("team_say", &json!({ "text": text }), &a);
        if r["ok"] == json!(true) { Ok(()) } else { Err(err_of(&r)) }
    }
    fn name_of(&self, user: &str) -> String { self.hub.name_of(user) }
    fn who(&self) -> String { self.who.clone() }
    fn known_user(&self, user: &str) -> bool { self.hub.account(user).is_some() }
    fn written_by(&self) -> &str { " by the hub" }
}

/// Today as `YYYY-MM-DD` (the notes' headings).
pub fn day() -> String { pgm_core_internal::time::iso_now().chars().take(10).collect() }

/// Writes the approved part of a draft as "<person> · Claude" (see [`pgm_plan_internal::write::write_plan`]).
pub fn write_plan(hub: &dyn AiHub, a: &Account, draft: &Value, approve: &[usize], with_changes: bool) -> Value {
    pgm_plan_internal::write::write_plan(&HubPlan::new(hub, &a.username), draft, approve, with_changes, &day())
}
