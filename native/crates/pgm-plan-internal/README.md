# pgm-plan-internal (PGM_Plan_Internal)

Planner mode as one library for the VibeRoller hub and Jcode PGM (T-301; board T-279 / T-281, T-276 / T-277 / T-278).
Pure logic: the only side effects are behind the `PlanHub` trait (board create / update / list, notes, chat).

| Module | What |
| --- | --- |
| `draft` | `normalize`: the AI's `plan_draft` arguments checked and read into the draft (owner always, no delete / complete, limits), `plans_path`, `slug` |
| `view` | `table`, `counts`: the parents / sub-tasks table the person reads |
| `flow` | `Decision` (`from_body` / `to_body`: the answer body `approve: "all" \| [i] \| false`, `comment`, `revise`, `changes`), `Flow` (the answers as `ask_user` questions), `Draft` (answered once), `approve` / `change_result` / `reject_result` (the tool results) |
| `write` | `PlanHub` and `write_plan`: source note, parents, sub-tasks `T-<parent> · n/m: title` with the parent label, blocked parents to backlog, changes to existing tasks only with "all" or their parent, owner map, chat summary |
| `planner` | `parse`, `existing`, `owner`, `workload`, `glossary`, roles from JSON / the assignments note / built-in, and `tool` (the five helper tools over `PlanHub`) |
| `helpers` | `HelperSet` (which helper tools a hub has, from its tool list), by-hand fallbacks, `steps_text` |

The hub implements `PlanHub` over its own stores (`pgm-ai-internal`); Jcode implements it over the signed-in hub's HTTP
routes. A change goes to pgm-libs first; both products copy the crate.
