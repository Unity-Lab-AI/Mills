//! File protection: only a file's owner may read it (it holds a token or a key).
//! Linux: mode 600. Windows: its own ACL instead of the folder's (the folder may be one every user of the machine can
//! read, e.g. C:\VibeRollerAIHost): the person, SYSTEM (a service) and Administrators, nobody else; done only when
//! someone else could read it.
use std::path::Path;

/// Only its owner may read the file from now on.
pub fn private(path: &Path) {
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)); }
    #[cfg(windows)]
    { if readable_by_others(path) { let _ = win_lock(path); } }
}
/// Is the file readable by anyone but its owner? (Linux: group / other bits; Windows: a Users / Everyone /
/// Authenticated Users entry in its ACL.)
pub fn readable_by_others(path: &Path) -> bool {
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::metadata(path).map(|m| m.permissions().mode() & 0o077 != 0).unwrap_or(false) }
    #[cfg(windows)]
    { match icacls(&[&path.display().to_string()]) { Some((true, out)) => ["BUILTIN\\Users", "Everyone", "Authenticated Users", "NT AUTHORITY\\INTERACTIVE"].iter().any(|w| out.contains(w)), _ => false } }
    #[cfg(not(any(unix, windows)))]
    { let _ = path; false }
}
#[cfg(windows)]
fn icacls(args: &[&str]) -> Option<(bool, String)> {
    use std::os::windows::process::CommandExt;
    let o = std::process::Command::new("icacls").args(args).creation_flags(0x0800_0000).stdin(std::process::Stdio::null()).output().ok()?;
    Some((o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)))
}
/// The ACL: inheritance off, full control for the person (when it is a person: a service runs as the machine account),
/// SYSTEM and Administrators (by their well-known ids, so it works on Windows in any language). Err(why) when icacls refused.
#[cfg(windows)]
pub fn win_lock(path: &Path) -> std::result::Result<(), String> {
    let me = std::env::var("USERNAME").unwrap_or_default();
    let dom = std::env::var("USERDOMAIN").unwrap_or_default();
    let p = path.display().to_string();
    let mut args: Vec<String> = vec![p, "/inheritance:r".into(), "/grant:r".into()];
    if !me.is_empty() && !me.ends_with('$') { args.push(if dom.is_empty() { format!("{me}:F") } else { format!("{dom}\\{me}:F") }); }
    args.push("*S-1-5-18:F".into()); // SYSTEM, by its well-known id (the name is translated on other-language Windows)
    args.push("*S-1-5-32-544:F".into()); // Administrators
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match icacls(&a) { Some((true, _)) => Ok(()), Some((false, out)) => Err(out.trim().lines().last().unwrap_or("").to_string()), None => Err("icacls did not start".into()) }
}
