//! **pgm-core-internal** = PGM_Core_Internal (PsyGraphMediaCoreInternal): the basics every PsyGraphMedia library and
//! program shares. Nothing here knows about any one product (VibeRoller is just its first user).
//! - [`Error`] / [`Result`]: an error meant for a person, with an HTTP-like status where one applies
//! - [`config`]: config.json overlaid with the machine's own config.local.json
//! - [`b64`] / [`unb64`], [`now_ms`]
//! - [`crypto`]: random bytes / ids, SHA-256, HMAC-SHA256 (constant-time check), AES-256-GCM, HKDF-SHA256
//! - [`time`]: ISO 8601 timestamps exactly as JavaScript's `new Date(ms).toISOString()`
//! - [`json`]: JSON files read and written atomically (and privately), and reading fields out of a JSON value
//! - [`fs`]: file protection: only the owner may read a file (Unix mode 600, Windows ACL via icacls)
//! - [`text`]: `encodeURIComponent` and friends
//! - [`notices`]: what a library has to tell the people running the program (a data file moved aside), drained by it
#![forbid(unsafe_code)]

pub mod config;
pub mod crypto;
pub mod fs;
pub mod json;
pub mod notices;
pub mod obs;
pub mod store;
pub mod text;
pub mod time;

/// Every error the PGM libraries hand out: a message meant for a person, plus an HTTP-like status where one applies.
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
