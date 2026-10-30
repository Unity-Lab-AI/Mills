//! **pgm-collab-internal** = PGM_Collab_Internal (PsyGraphMediaCollaborationInternal): working together.
//! - [`board`]: a task board people and AI agents work at the same time: tasks that are calendar entries too,
//!   time-bounded claims, a revision per task, tombstones, an activity log, month-end aware repeats; the agent tools;
//!   one JSON file, written atomically
//! - [`notes`]: markdown notes in folders (real .md files), case-insensitive names, who saved each last; the agent tools
//! - [`worktree`]: projects (a git repository each) and one worktree per person, made on first use
//! - [`forgejo`]: a Forgejo instance: people's tokens, the board ↔ issues mirror

pub mod codefind;
pub mod board;
pub mod forgejo;
pub mod notes;
pub mod worktree;
