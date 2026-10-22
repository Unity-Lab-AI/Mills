//! **pgm-ai-internal** = PGM_AI_Internal (PsyGraphMediaAIInternal): everything AI.
//! - [`ai_hosts`]: machines that lend their AI to a hub (their Claude / ChatGPT login, local models, an assistant's
//!   engines) by connecting out: the registry, the hub side of the host link (hello, jobs, replies, streams, events,
//!   updates), sharing with allowed users, engines, the PC host, caps
//! - [`usage`]: usage windows (5 hours, 7 days) per source, from the events and the rate-limit headers
//! - [`unity_time`]: the assistant's clock and calculator: time zones, "in 2 minutes" / dates, a safe calculator
//! - [`unity_web`]: the assistant's web tools: search, news, weather, YouTube, reading a page (the address it checked,
//!   capped)
//! Providers, inference APIs, Work runs, the assistant's brain and the host runner join as they move (board T-140).

pub mod ai_hosts;
pub mod unity_time;
pub mod unity_web;
pub mod usage;
