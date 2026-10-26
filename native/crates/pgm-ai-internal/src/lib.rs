//! **pgm-ai-internal** = PGM_AI_Internal (PsyGraphMediaAIInternal): everything AI.
//! - [`ai_hosts`]: machines that lend their AI to a hub (their Claude / ChatGPT login, local models, an assistant's
//!   engines) by connecting out: the registry, the hub side of the host link (hello, jobs, replies, streams, events,
//!   updates), sharing with allowed users, engines, the PC host, caps
//! - [`usage`]: usage windows (5 hours, 7 days) per source, from the events and the rate-limit headers
//! - [`unity_time`]: the assistant's clock and calculator: time zones, "in 2 minutes" / dates, a safe calculator
//! - [`unity_web`]: the assistant's web tools: search, news, weather, YouTube, reading a page (the address it checked,
//!   capped)
//! - [`openai`]: providers (keys, endpoints, logins; admin-only kinds) and the inference APIs: OpenAI- and
//!   Anthropic-compatible /v1 (plain + streaming, format conversion), per-person API keys
//! - [`work`]: AI coding runs (Claude Code here, relayed to a host's login, or on an older host), their log + attach /
//!   follow, the MCP tool relays (pc / board / team), shared sessions, projects, past sessions
//! - [`planner`]: planner mode's deterministic helpers (parse an idea list, find what already exists, roles and owners,
//!   the glossary) and the team MCP tools `plan_parse`, `plan_existing`, `plan_roles`, `plan_owner`, `plan_glossary`
//! - [`unity`], [`unity_brain`], [`unity_routes`]: the assistant: her tools, her brain (a local model's tool loop or
//!   Claude), confirmations, voice / speech / pictures, her section's routes, the chat's AI member and its queue
//! - [`hub`]: what all of this needs from the program around it ([`hub::AiHub`])
//! - [`host`]: the AI host program's side (the job loop, relays, Work on the host, its MCP tools, its folder + settings)
//! - [`machine_jobs`], [`machine_transfer`]: the hub's side of linked machines' tools (T-311): the signed job queue a machine's
//!   poll drains, replies matched to calls, the routing of a run's PC / machine tool call to its owner's online machine, and
//!   big-file tickets with the per-account transfer area (chunks straight to disk)
//! - [`local_proxy`]: a local API proxy (127.0.0.1 → a hub's /v1 with a person's key; no web page can use it)
//! - [`fj`]: `fj` (the Forgejo CLI) signed in as the person in their Work runs (their own keys file + wrapper)
//! - [`harness`]: the `Harness` trait (Claude Code, Jcode PGM): a run's command, resume arguments and the mapping of
//!   each stdout line onto the one hub event shape ([`harness::HubEvent`])
//! - [`jcode`]: Jcode PGM as a Work harness (a `jcode` provider: `jcodepgm run --ndjson` in the worktree, a private
//!   jcode home per person, its events mapped onto the page's event shape)

pub mod ai_hosts;
pub mod fj;
pub mod gitpanel;
pub mod harness;
pub mod host;
pub mod hub;
pub mod jcode;
pub mod local_proxy;
pub mod machine_jobs;
pub mod machine_transfer;
pub mod openai;
pub mod plan;
pub mod planner;
pub mod projects;
pub mod questions;
pub mod sessions;
pub mod unity;
pub mod unity_brain;
pub mod unity_routes;
pub mod unity_time;
pub mod unity_web;
pub mod usage;
pub mod work;
