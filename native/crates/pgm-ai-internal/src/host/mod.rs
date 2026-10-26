//! The AI host program's side: a machine that lends its AI to a hub (its Claude / ChatGPT logins, local models, an
//! assistant's engines) by connecting out (pgm-net-internal's host link) and answering the hub's jobs.
//! - [`runner`]: the host itself: one per folder, hello, the job loop, relays, PC tools, updating its own binary
//! - [`relay`]: a provider / login relay (Claude, Codex logins refreshed here; only the paths that belong)
//! - [`work`]: a Work run on this machine (the older protocol, for hubs that do not relay)
//! - [`mcp`]: the MCP tool server a run's tools talk to
//! - [`pin`]: the hub signing key this host pinned, and the check of the hub signed PC jobs (T-308)
//! - [`folder`]: the host folder and the files in it (settings, status, lock, log, switches)
//! - [`settings`]: ai-node.json, read and written with the control app's keys, defaults and checks

pub mod control;
pub mod folder;
pub mod mcp;
pub mod pin;
pub mod relay;
pub mod runner;
pub mod settings;
pub mod work;
