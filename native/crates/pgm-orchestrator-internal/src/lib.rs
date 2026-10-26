//! **pgm-orchestrator-internal** = PGM_Orchestrator_Internal: the policy half of `/pgm-orchestrator` (Jcode-Mods
//! DECISIONS.md A1), pure Rust, no I/O, so every rule is a unit test. The orchestrating model plans, writes exact
//! briefs and reviews; cheap workers do the labour. The rules that came out of real failures in the fork stay:
//!
//! 1. load is compared before the user's rank (ranking first starves spreading);
//! 2. every task kind needs at least two candidates in a profile (nothing to swap to otherwise);
//! 3. a refusal is permanent for that worker and task kind, not a cooldown;
//! 4. our own cancellations are not strikes;
//! 5. unroutable means fail, never fall back to the orchestrator's own (paid) model;
//! 6. never fill a worker's window: a fraction cap and an absolute headroom floor both apply;
//! 7. jcode never picks worker models: the user lists them in a profile (free-only, one provider, paid-only, mixed);
//! 8. "free and large" is not "usable": a worker says what it is good at;
//! 9. trust never loosens safety: it only says whether repo-mutating work may be assigned.
//!
//! The plugin around it (Jcode PGM `pgm/plugins/pgm-orchestrator`) reads profiles from config, keeps the health
//! ledger per session, routes `model = "auto" | "auto:<kind>"` swarm spawns through [`select`], and renders
//! [`directive`] + [`roster_briefing`] into the prompt while the mode is on.

pub mod attempts;
pub mod catalog;
pub mod dispatch;
pub mod health;
pub mod local;
pub mod profile;
pub mod prompt;
pub mod route_error;
pub mod select;
pub mod spend;
pub mod tier;
pub mod windows;

pub use attempts::{next_attempt, Attempt, AttemptOutcome, NextStep, Timeline, Unit};
pub use catalog::{
    correct_context_window, is_local_provider, is_media_model, prove_free, CatalogEntry,
};
pub use dispatch::{DispatchError, Plan};
pub use local::{local_roster, merge_local, LocalModel, LocalRoster};
pub use windows::{apply_catalog_windows, ProviderCatalog, WindowReport};
pub use route_error::{classify_route_error, needs_context_at_least, RouteError};
pub use spend::{cost_micros, SpendLedger, Usage, WorkerSpend};

pub use health::{Health, Outcome};
pub use profile::{Mode, Profile, TaskKind, Trust, Worker};
pub use prompt::{cost_ladder, directive, roster_briefing};
pub use tier::{capability_tier, sort_cheapest_first, Tier};
pub use select::{infer_task_kind, select, Selection};
