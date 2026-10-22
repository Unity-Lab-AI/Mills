//! `fj` (the Forgejo CLI, forgejo-cli) in a Work run, signed in as the person whose run it is.
//! fj keeps its logins in one keys file per operating-system account (Linux: `$XDG_DATA_HOME/forgejo-cli/keys.json`,
//! Windows: `%APPDATA%\forgejo-cli\forgejo-cli\data\keys.json`), so on a shared machine everyone's runs would be one
//! login. A run is one person's, so it gets its own: a folder of theirs holding that file (their token from Settings →
//! My Forgejo, owner-only) and a tiny `fj` wrapper first on the PATH that points fj at it (XDG_DATA_HOME / APPDATA set
//! for the fj process alone; nothing else in the run sees them). Without fj on the machine, or without a token, nothing
//! is made and fj stays whatever the machine has. The hub does this for runs here ([`crate::work`]), a host for the
//! runs it takes ([`crate::host::work`]); bridge/fj.js is the same for the JavaScript hub and host.
use pgm_core_internal::json::{read_json, write_json};
use serde_json::json;
use std::path::{Path, PathBuf};

/// The wrapper's folder name, so the real fj is never one of our wrappers.
const WRAP: &str = "fj-bin";

/// The real fj on the PATH.
fn find_fj() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["fj.exe", "fj.cmd", "fj.bat", "fj"] } else { &["fj"] };
    for d in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if d.file_name().map(|n| n == WRAP).unwrap_or(false) { continue; }
        for n in names { let p = d.join(n); if p.is_file() { return Some(p); } }
    }
    None
}
fn host_of(url: &str) -> String { url.trim().trim_start_matches("https://").trim_start_matches("http://").split('/').next().unwrap_or("").to_string() }
fn sq(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) } // a sh single-quoted word

/// Writes the person's keys file + wrapper under `dir` and gives the environment their run gets (the PATH with the
/// wrapper first); empty when nothing applies. Never fails: a run must start whatever happens here.
pub fn env(dir: &Path, url: &str, token: &str, login: &str) -> Vec<(String, String)> {
    let host = host_of(url);
    if token.is_empty() || host.is_empty() { return vec![]; }
    let Some(real) = find_fj() else { return vec![] };
    let data = if cfg!(windows) { dir.join("forgejo-cli").join("forgejo-cli").join("data") } else { dir.join("forgejo-cli") };
    if pgm_core_internal::fs::private_dir(dir).is_err() || std::fs::create_dir_all(&data).is_err() { return vec![]; }
    let keys = data.join("keys.json");
    let mut cur = read_json(&keys).filter(|v| v.is_object()).unwrap_or_else(|| json!({}));
    if !cur["hosts"].is_object() { cur["hosts"] = json!({}); }
    cur["hosts"][&host] = json!({ "type": "Application", "token": token });
    if !cur["aliases"].is_object() { cur["aliases"] = json!({}); }
    if !cur["default_ssh"].is_array() { cur["default_ssh"] = json!([]); }
    write_json(&keys, &cur);
    pgm_core_internal::fs::private(&keys);
    let wrap = dir.join(WRAP);
    if std::fs::create_dir_all(&wrap).is_err() { return vec![]; }
    let (d, r) = (dir.display().to_string(), real.display().to_string());
    let var = if cfg!(windows) { "APPDATA" } else { "XDG_DATA_HOME" };
    let who = if login.is_empty() { host.as_str() } else { login };
    if std::fs::write(wrap.join("fj"), format!("#!/bin/sh\n# fj as {who}: its keys file under {d} (made by VibeRoller for this person's Work runs)\n{var}={} exec {} \"$@\"\n", sq(&d), sq(&r))).is_err() { return vec![]; }
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(wrap.join("fj"), std::fs::Permissions::from_mode(0o755)); }
    if cfg!(windows) { let _ = std::fs::write(wrap.join("fj.cmd"), format!("@echo off\r\nset \"APPDATA={d}\"\r\n\"{r}\" %*\r\n")); }
    let path = std::env::join_paths(std::iter::once(wrap).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()))).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    if path.is_empty() { return vec![]; }
    vec![("PATH".into(), path)]
}
