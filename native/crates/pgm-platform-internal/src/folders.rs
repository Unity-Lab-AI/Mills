//! A program's folders.
use std::path::PathBuf;

/// The person's home folder (USERPROFILE on Windows, HOME elsewhere).
pub fn home() -> PathBuf { std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default() }
/// This program's own path, as text ("" when it cannot be told).
pub fn exe() -> Option<String> { std::env::current_exe().ok().map(|p| p.display().to_string()) }
