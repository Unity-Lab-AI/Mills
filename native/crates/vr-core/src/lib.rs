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

/// Every error vr-core hands out: a message meant for a person, plus an HTTP-like status where one applies.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Msg(String),
    #[error("{message}")]
    Status { status: u16, message: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Http(#[from] reqwest::Error),
}
impl Error {
    pub fn msg(m: impl Into<String>) -> Self { Error::Msg(m.into()) }
    pub fn status(status: u16, m: impl Into<String>) -> Self { Error::Status { status, message: m.into() } }
    /// The HTTP status this error stands for (502 when it has none of its own).
    pub fn code(&self) -> u16 { match self { Error::Status { status, .. } => *status, _ => 502 } }
}
pub type Result<T> = std::result::Result<T, Error>;

/// Standard base64 (with padding), as the browser's btoa and Node's Buffer use.
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
/// Standard base64 back to bytes; also accepts the URL-safe alphabet and missing padding.
pub fn unb64(s: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    let t: String = s.trim().chars().map(|c| match c { '-' => '+', '_' => '/', c => c }).filter(|c| *c != '=').collect();
    base64::engine::general_purpose::STANDARD_NO_PAD.decode(t.as_bytes()).map_err(|e| Error::msg(format!("bad base64: {e}")))
}
/// Milliseconds since 1970, like Date.now().
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_round_trip_and_url_safe() {
        let b = [0u8, 1, 2, 250, 251, 252, 253, 254, 255];
        let s = super::b64(&b);
        assert_eq!(super::unb64(&s).unwrap(), b);
        assert_eq!(super::unb64(&s.replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()).unwrap(), b);
    }
}
