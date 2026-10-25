//! Planner helpers (T-276 intake, T-277 owners, T-278 vocabulary; team note plans/planner-mode.md § 2 to 5): the
//! deterministic parts of planning, so the planner AI does not guess them. Since T-301 the logic (parse, existing,
//! roles, owner, glossary, the fallbacks) is the library `pgm-plan-internal::planner`, shared with Jcode PGM; this file
//! is the hub side of the five team MCP tools: [`tool`] runs them over the hub's own stores ([`crate::plan::HubPlan`]).
//! Nothing here writes anything; the planner shows its draft and the person approves it first (rule 7).
use crate::hub::AiHub;
use serde_json::Value;

pub use pgm_plan_internal::planner::*;

/// The five planner tools of the team MCP server (any session may call them: they only read). `None` = not one of them.
pub fn tool(hub: &dyn AiHub, account: &str, name: &str, args: &Value) -> Option<Value> {
    pgm_plan_internal::planner::tool(&crate::plan::HubPlan::new(hub, account), name, args, pgm_core_internal::now_ms() as i64)
}
