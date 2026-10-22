//! vr-core: what every VibeRoller program on a machine shares (the AI host, the desktop app, the hub).
//!
//! Each module matches its JavaScript reference byte for byte where the two talk to each other, so a Rust program
//! works with a JavaScript hub / host / page and the other way round (LAWS.md: forwards and backwards compatible):
//! - [`crypto`]: the AI host session envelope (HKDF + AES-256-GCM, bridge/ai-nodes.js + ai-node.js), the signed update
//!   manifest (HMAC-SHA256), and the chat's end-to-end encryption (ECDH P-256, v3 / v4 messages, files, own seals:
//!   web/index.html)
//! - [`settings`]: ai-node.json, read and written with the same keys, defaults and checks as the control app
//!   (bridge/vr-host.js settingsList); unknown keys are kept as they are
//! - [`host`]: the AI host protocol types (/node/hello, /node/poll with its heartbeat, jobs, /node/reply, /node/stream,
//!   /node/events, /node/update)
//! - [`hub`]: a client for the hub's HTTP API (sign-in, /api/check, /api/version, Files, the board, any route)
//! - [`update`]: verifying a signed update and swapping a program's own binary in place, with a backup
#![forbid(unsafe_code)]

pub mod crypto;
pub mod host;
pub mod hub;
pub mod settings;
pub mod update;

/// The version of this crate (the native workspace's version, separate from the project's versions.json).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// The basics live in pgm-core-internal (PGM_Core_Internal) since 4.2.0; vr-core keeps their old names.
pub use pgm_core_internal::{b64, now_ms, unb64, Error, Result};
