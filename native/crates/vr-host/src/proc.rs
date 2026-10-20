//! Starting other programs the same way on Windows and Linux: found on the PATH (on Windows with PATHEXT, so the npm
//! shims claude.cmd / codex.cmd are found; Rust quotes arguments for .cmd files safely), never with a window of its own.
use std::path::PathBuf;

/// The full path of a program on the PATH, or None.
pub fn which(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(name);
    if p.components().count() > 1 || p.is_absolute() { return if p.exists() { Some(p) } else { None }; }
    let exts: Vec<String> = if cfg!(windows) { std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into()).split(';').map(|s| s.to_ascii_lowercase()).collect() } else { vec![String::new()] };
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for ext in &exts {
            let f = dir.join(format!("{name}{ext}"));
            if f.is_file() { return Some(f); }
        }
        if cfg!(windows) && dir.join(name).is_file() && name.contains('.') { return Some(dir.join(name)); }
    }
    None
}
/// A tokio Command for `name` (resolved on the PATH), with no console window on Windows, killed when dropped.
pub fn command(name: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(which(name).unwrap_or_else(|| PathBuf::from(name)));
    #[cfg(windows)]
    { c.creation_flags(crate::winsys::CREATE_NO_WINDOW); }
    c.kill_on_drop(true);
    c
}
/// The same for a blocking std Command.
pub fn std_command(name: &str) -> std::process::Command {
    #[allow(unused_mut)] // only Windows changes it
    let mut c = std::process::Command::new(which(name).unwrap_or_else(|| PathBuf::from(name)));
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(crate::winsys::CREATE_NO_WINDOW); }
    c
}
/// Runs a program and gives (success, stdout + stderr).
pub fn run(name: &str, args: &[&str]) -> (bool, String) {
    match std_command(name).args(args).output() {
        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (false, e.to_string()),
    }
}
