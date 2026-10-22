# pgm-collab-internal: PGM_Collab_Internal (PsyGraphMediaCollaborationInternal)

Working together, people and AI agents at the same time.

- `board::Board::new(dir)`: one JSON file, written atomically; tasks that are calendar entries too (`parse_due`,
  `next_due`, `day_key`), time-bounded claims, a revision per task, tombstones, an activity log; the agent tools
  (`tool_op`, `route_op`)
- `notes::Notes::new(dir)`: markdown notes in folders (real .md files), case-insensitive names, who saved each last
- `worktree::Worktrees::new(root, cfg)`: projects (a git repository each) and one worktree per person; `git(dir, args)`
- `forgejo::Mirror::new(root, cfg)`: people's Forgejo tokens and the board ↔ issues mirror

## Use it in another project

```rust
use pgm_collab_internal::{board::Board, notes::Notes};
let board = Board::new("data/Board".into());
let notes = Notes::new("data/Notes".into());
```
