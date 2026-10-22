//! Version stamps: what a checkout runs (its versions.json project version + its git commit), and a build name read
//! once at start (version + "+" + the commit's first 7), so a running program keeps naming the build it started on.
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Mutex;

/// (version from `<checkout>/versions.json` "project", else "0.0.0"; the full commit of HEAD, else "").
pub fn release_of(checkout: &Path) -> (String, String) {
    let version = std::fs::read_to_string(checkout.join("versions.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["project"].as_str().map(String::from)).unwrap_or_else(|| "0.0.0".into());
    let commit = std::process::Command::new("git").args(["rev-parse", "HEAD"]).current_dir(checkout).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null()).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    (version, commit)
}
/// The build a program started on: read once (the first time it is asked), then always the same.
pub struct Build { at: Mutex<Option<(String, Value, String)>> }
impl Default for Build { fn default() -> Self { Build { at: Mutex::new(None) } } }
impl Build {
    fn load(&self, checkout: &Path) -> (String, Value, String) {
        let mut b = self.at.lock().unwrap();
        if b.is_none() {
            let (v, c) = release_of(checkout);
            let mods = std::fs::read_to_string(checkout.join("versions.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| v["modules"].clone()).unwrap_or(json!({}));
            *b = Some((if c.is_empty() { v } else { format!("{v}+{}", &c[..7.min(c.len())]) }, mods, c));
        }
        b.clone().expect("loaded")
    }
    /// (build name, the modules' versions from versions.json)
    pub fn get(&self, checkout: &Path) -> (String, Value) { let b = self.load(checkout); (b.0, b.1) }
    /// The commit it started from.
    pub fn commit(&self, checkout: &Path) -> String { self.load(checkout).2 }
}
