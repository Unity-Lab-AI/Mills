//! The planner helper tools of the hub (T-276 / T-277 / T-278: `plan_glossary`, `plan_roles`, `plan_parse`,
//! `plan_existing`, `plan_owner`) in the order a planner uses them, then `plan_draft`. They are exact where guessing is
//! not, but a hub that is older than the helpers does not have them, and a Jcode session may not reach the hub's tools
//! at all. So every step has what to do by hand, the prompt says which tools are there (or to try them and fall back at
//! an "unknown tool" answer), and a missing tool is never a stop.

use serde_json::Value;

/// One helper step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Helper {
    pub name: &'static str,
    /// What it does, in the planner's order.
    pub does: &'static str,
    /// What to do instead when the tool is not there.
    pub by_hand: &'static str,
}

/// The helpers, in the order they are used.
pub const HELPERS: [Helper; 5] = [
    Helper {
        name: "plan_glossary",
        does: "the team's words and decisions (\"blocked on X\" means based on X)",
        by_hand: "read the team note plans/glossary.md (notes_read) when you can, else ask",
    },
    Helper {
        name: "plan_roles",
        does: "who does what, and each person's open workload",
        by_hand: "read plans/task-assignments.md (section Roles) and count each person's open tasks with board_list",
    },
    Helper {
        name: "plan_parse",
        does: "splits the person's rough list into ideas exactly (`---` blocks, person sections, short and long versions, run-on chat)",
        by_hand: "split it yourself at each `---` block, person section and new wish, merge a short and a long version of the same list, and keep the person's words word for word as sourceText",
    },
    Helper {
        name: "plan_existing",
        does: "what of an idea already exists on the board",
        by_hand: "search board_list (done tasks of the last 30 days too) for each idea and scope the task to what is left",
    },
    Helper {
        name: "plan_owner",
        does: "the owner of an idea or sub-task (a marker, then the role, then the lighter workload; it never answers empty)",
        by_hand: "pick by the person's marker, then the role, then the lighter workload; when it stays open, ask_user instead of guessing",
    },
];

/// Which helpers a hub has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperSet {
    have: [bool; 5],
}

/// The bare tool name: `mcp__team__plan_parse` -> `plan_parse`.
fn bare(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

impl HelperSet {
    pub fn all() -> HelperSet {
        HelperSet { have: [true; 5] }
    }

    pub fn none() -> HelperSet {
        HelperSet { have: [false; 5] }
    }

    /// From the names of the tools that exist (bare or `mcp__<server>__<tool>`).
    pub fn from_tool_names<S: AsRef<str>>(names: &[S]) -> HelperSet {
        let mut have = [false; 5];
        for (i, h) in HELPERS.iter().enumerate() {
            have[i] = names.iter().any(|n| bare(n.as_ref()) == h.name);
        }
        HelperSet { have }
    }

    /// From a hub's tool list: an MCP `tools/list` answer (`{tools: [{name}]}`, also inside `result`), a list of such
    /// tools or a list of names. Anything else (an error, an empty answer) means none.
    pub fn from_tools_list(v: &Value) -> HelperSet {
        let list = v.get("tools").or_else(|| v.get("result").and_then(|r| r.get("tools"))).unwrap_or(v);
        let names: Vec<String> = list
            .as_array()
            .map(|a| a.iter().filter_map(|t| t.as_str().or_else(|| t.get("name").and_then(Value::as_str)).map(String::from)).collect())
            .unwrap_or_default();
        Self::from_tool_names(&names)
    }

    pub fn has(&self, name: &str) -> bool {
        HELPERS.iter().position(|h| h.name == bare(name)).map(|i| self.have[i]).unwrap_or(false)
    }

    /// The helpers this hub lacks, in order.
    pub fn missing(&self) -> Vec<&'static str> {
        HELPERS.iter().enumerate().filter(|(i, _)| !self.have[*i]).map(|(_, h)| h.name).collect()
    }

    /// Take a helper off the list (a call to it answered "unknown tool").
    pub fn lose(&mut self, name: &str) {
        if let Some(i) = HELPERS.iter().position(|h| h.name == bare(name)) {
            self.have[i] = false;
        }
    }
}

/// The tool an answer says is unknown, when it is a planner helper: Jcode's `Unknown tool: plan_parse. ... Available
/// tools: ...` and the hub's `Unknown team tool: plan_parse` (any case, with or without the `mcp__server__` prefix).
pub fn unknown_helper(answer: &str) -> Option<&'static str> {
    let low = answer.to_ascii_lowercase();
    let (at, len) = ["unknown tool", "unknown team tool"].iter().find_map(|p| low.find(p).map(|i| (i, p.len())))?;
    let after = answer[at + len..].trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    HELPERS.iter().find(|h| h.name == bare(&name)).map(|h| h.name)
}

/// What to do by hand for a helper, `None` for a name that is not one.
pub fn by_hand(name: &str) -> Option<&'static str> {
    HELPERS.iter().find(|h| h.name == bare(name)).map(|h| h.by_hand)
}

/// The planner's working order for the prompt: the helpers that exist, each with what it is for, a by-hand line for each
/// that is missing, and `plan_draft` at the end (always: Jcode has its own).
pub fn steps_text(set: &HelperSet) -> String {
    let mut out = String::from("Work in this order. Each helper tool below only reads and is exact where guessing is not; use it when it is in your tool list, and when a call answers \"unknown tool\" (an older hub) do that step by hand as written and go on:\n");
    for (i, h) in HELPERS.iter().enumerate() {
        if set.have[i] {
            out += &format!("{}) {}: {}. By hand, if it is not there: {}.\n", i + 1, h.name, h.does, h.by_hand);
        } else {
            out += &format!("{}) {} is not available here: {}.\n", i + 1, h.name, h.by_hand);
        }
    }
    out += "6) plan_draft: show the whole plan (parents with owner, priority, status, blocked on; sub-tasks; changes to existing tasks; what is not planned and why; the person's words as sourceText). The person approves all, some, asks for a change (you get their words: revise and call plan_draft again) or rejects; only then is anything written, and the result lists the new task ids. Do not write the board yourself: task_create, task_update, task_move and task_delete are refused here.";
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_order_is_the_hubs_order() {
        let names: Vec<&str> = HELPERS.iter().map(|h| h.name).collect();
        assert_eq!(names, ["plan_glossary", "plan_roles", "plan_parse", "plan_existing", "plan_owner"]);
        let t = steps_text(&HelperSet::all());
        let at = |n: &str| t.find(n).unwrap_or_else(|| panic!("{n}"));
        assert!(at("plan_glossary") < at("plan_roles") && at("plan_roles") < at("plan_parse") && at("plan_parse") < at("plan_existing") && at("plan_existing") < at("plan_owner") && at("plan_owner") < at("6) plan_draft"));
    }

    #[test]
    fn a_hub_with_the_tools_is_read_from_its_tool_list() {
        let list = json!({ "result": { "tools": [{ "name": "team_say" }, { "name": "plan_glossary" }, { "name": "plan_roles" }, { "name": "plan_parse" }, { "name": "plan_existing" }, { "name": "plan_owner" }, { "name": "plan_draft" }] } });
        assert_eq!(HelperSet::from_tools_list(&list), HelperSet::all());
        assert!(HelperSet::all().missing().is_empty());
    }

    #[test]
    fn an_older_hub_lacks_them_and_every_step_has_a_by_hand_line() {
        // a hub from before T-276: only the first of the family, and the answer of a hub that knows no list
        let old = json!({ "tools": [{ "name": "mcp__team__plan_glossary" }, { "name": "team_say" }] });
        let set = HelperSet::from_tools_list(&old);
        assert!(set.has("plan_glossary") && !set.has("plan_parse"));
        assert_eq!(set.missing(), ["plan_roles", "plan_parse", "plan_existing", "plan_owner"]);
        let t = steps_text(&set);
        assert!(t.contains("plan_parse is not available here: split it yourself"), "{t}");
        assert!(t.contains("6) plan_draft"), "the draft is still the way to the board: {t}");
        for h in HELPERS {
            assert!(!h.by_hand.is_empty() && by_hand(h.name) == Some(h.by_hand));
        }
        assert_eq!(HelperSet::from_tools_list(&json!({ "error": "no list" })), HelperSet::none());
        assert_eq!(HelperSet::from_tools_list(&json!(["plan_owner"])).missing().len(), 4);
        assert_eq!(by_hand("mcp__team__plan_owner"), by_hand("plan_owner"));
        assert_eq!(by_hand("task_get"), None);
    }

    #[test]
    fn an_unknown_tool_answer_names_the_helper_to_do_by_hand() {
        let jcode = "Unknown tool: plan_parse. Available tools: agentgrep, ask_user, bash. ";
        assert_eq!(unknown_helper(jcode), Some("plan_parse"));
        assert_eq!(unknown_helper("Unknown tool: mcp__team__plan_owner."), Some("plan_owner"));
        assert_eq!(unknown_helper("Unknown team tool: plan_existing"), Some("plan_existing"));
        assert_eq!(unknown_helper("unknown tool: PLAN_ROLES"), None, "names are exact");
        assert_eq!(unknown_helper("Unknown tool: ToolSearch."), None);
        assert_eq!(unknown_helper("fine"), None);
        let mut set = HelperSet::all();
        set.lose("plan_parse");
        assert_eq!(set.missing(), ["plan_parse"]);
    }
}
