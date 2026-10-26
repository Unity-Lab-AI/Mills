# pgm-workflow-command-internal

PGM_WorkflowCommand_Internal: the one parser of the `/hyperwork <name> [--project P] [--on runner] [--version V] <prompt>`
line (`/hw` is the short form), so the VibeRoller page, Hyper Prod Man and Jcode PGM read a line the same way (T-207,
plans/hyper-workflows.md § 10.2), and the "did you mean" helper (`nearness`, `nearest`) for a name that matches no
workflow (a product never runs "the closest" by guess).

- `parse(line) -> Parsed::{No, Typing(name), Command(Command), Bad(message)}`: `Typing` is the type-ahead's text (a name
  still being typed, no space after it yet).
- Depends on serde alone: a TUI or a page build can take it without the engine or an async runtime.
- The engine crate re-exports it as `pgm_workflow_engine_internal::command`, so nothing that used that path changes.
- Mirrored by copy into Jcode PGM's `pgm/libs/` (the same file, the same version); it goes to pgm-libs with the Hyper
  Workflows release (NAMES.md).
