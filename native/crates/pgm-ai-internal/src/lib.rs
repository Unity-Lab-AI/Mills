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
//! - [`unity`], [`unity_brain`], [`unity_routes`]: the assistant: her tools, her brain (a local model's tool loop or
//!   Claude), confirmations, voice / speech / pictures, her section's routes, the chat's AI member and its queue
//! - [`hub`]: what all of this needs from the program around it ([`hub::AiHub`])
//! - [`host`]: the AI host program's side (the job loop, relays, Work on the host, its MCP tools, its folder + settings)
//! - [`local_proxy`]: a local API proxy (127.0.0.1 → a hub's /v1 with a person's key; no web page can use it)
//! - [`fj`]: `fj` (the Forgejo CLI) signed in as the person in their Work runs (their own keys file + wrapper)

pub mod ai_hosts;
pub mod fj;
pub mod host;
pub mod hub;
pub mod local_proxy;
pub mod openai;
pub mod unity;
pub mod unity_brain;
pub mod unity_routes;
pub mod unity_time;
pub mod unity_web;
pub mod usage;
pub mod work;
