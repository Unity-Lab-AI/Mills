//! vr-core: what every VibeRoller program on a machine shares (the AI host, the desktop app, later the hub).
//!
//! Planned modules (board T-125), each matching its JavaScript reference so old and new keep talking:
//! - `hub`: the hub's HTTP API client (sign-in, /api/check, files, board, notes) — bridge/server.js
//! - `host`: the AI host protocol (/node/hello, /poll with its heartbeat, /reply, /stream, /node/update) — bridge/ai-node.js, bridge/ai-nodes.js
//! - `crypto`: the chat's end-to-end encryption (ECDH P-256, HKDF, AES-GCM v3 / v4) and the host handshake — web/index.html, bridge/common.js
//! - `settings`: ai-node.json and the desktop settings, read and written with the same keys — bridge/vr-host.js
//! - `update`: the signed update manifest (HMAC-SHA256 with the host's secret) and binary swaps — bridge/ai-node.js checkUpdate
#![forbid(unsafe_code)]

/// The version of this crate (the native workspace's version, separate from the project's versions.json).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn has_a_version() {
        assert!(!super::VERSION.is_empty());
    }
}
