//! The native-only features of the Tools window, on this computer and inside the person's projects folder:
//! files without browser permission prompts, an integrated terminal (a real pseudo-terminal), a build / run runner,
//! git with Forgejo sign-in. Nothing here is reachable from the hub's page (bridge: only the Tools window, which is
//! part of this app, may call these; see main.rs capabilities).
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

/// A path inside the projects folder (never above it).
pub fn inside(root: &str, rel: &str) -> Result<PathBuf, String> {
    if root.is_empty() { return Err("Choose your projects folder first (Settings in this window).".into()); }
    let root = PathBuf::from(root);
    let segs: Vec<&str> = rel.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").collect();
    if segs.contains(&"..") { return Err("Paths cannot go above the projects folder.".into()); }
    Ok(segs.iter().fold(root, |p, s| p.join(s)))
}
pub fn fs_list(root: &str, rel: &str) -> Result<Value, String> {
    let d = inside(root, rel)?;
    let mut out: Vec<Value> = std::fs::read_dir(&d).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).map(|e| {
        let m = e.metadata().ok();
        json!({ "name": e.file_name().to_string_lossy(), "dir": m.as_ref().map(|m| m.is_dir()).unwrap_or(false), "size": m.as_ref().map(|m| m.len()).unwrap_or(0),
            "modified": m.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0) })
    }).collect();
    out.sort_by(|a, b| (!a["dir"].as_bool().unwrap_or(false), a["name"].as_str().unwrap_or("").to_lowercase()).cmp(&(!b["dir"].as_bool().unwrap_or(false), b["name"].as_str().unwrap_or("").to_lowercase())));
    Ok(json!(out))
}
pub fn fs_read(root: &str, rel: &str) -> Result<Value, String> {
    let f = inside(root, rel)?;
    let m = std::fs::metadata(&f).map_err(|e| e.to_string())?;
    if m.len() > 10 << 20 { return Err("That file is larger than 10 MB.".into()); }
    let b = std::fs::read(&f).map_err(|e| e.to_string())?;
    match String::from_utf8(b) { Ok(t) if !t.contains('\0') => Ok(json!({ "text": t })), _ => Ok(json!({ "binary": true, "size": m.len() })) }
}
pub fn fs_write(root: &str, rel: &str, text: &str) -> Result<(), String> {
    let f = inside(root, rel)?;
    if rel.trim().is_empty() { return Err("Missing file name.".into()); }
    if let Some(p) = f.parent() { std::fs::create_dir_all(p).map_err(|e| e.to_string())?; }
    std::fs::write(&f, text).map_err(|e| e.to_string())
}
pub fn fs_delete(root: &str, rel: &str) -> Result<(), String> {
    let f = inside(root, rel)?;
    if rel.trim().is_empty() { return Err("Refusing to delete the whole projects folder.".into()); }
    if f.is_dir() { std::fs::remove_dir_all(&f) } else { std::fs::remove_file(&f) }.map_err(|e| e.to_string())
}
pub fn fs_mkdir(root: &str, rel: &str) -> Result<(), String> { std::fs::create_dir_all(inside(root, rel)?).map_err(|e| e.to_string()) }

// ---------- the terminal ----------
pub struct Term { writer: Box<dyn Write + Send>, master: Box<dyn MasterPty + Send>, child: Box<dyn portable_pty::Child + Send + Sync> }
#[derive(Default)]
pub struct Terms { pub map: Mutex<HashMap<u32, Term>>, next: Mutex<u32> }
fn shell() -> CommandBuilder {
    if cfg!(windows) { let mut c = CommandBuilder::new(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())); if which("pwsh").is_some() { c = CommandBuilder::new("pwsh"); c.arg("-NoLogo"); } c }
    else { let mut c = CommandBuilder::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())); c.arg("-l"); c }
}
fn which(n: &str) -> Option<PathBuf> { std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).flat_map(|d| [d.join(n), d.join(format!("{n}.exe"))]).find(|p| p.is_file()) }
/// Opens a terminal in `cwd` (inside the projects folder); its output arrives as "term" events { id, data }.
pub fn term_open(app: &AppHandle, terms: &Arc<Terms>, root: &str, rel: &str, cols: u16, rows: u16) -> Result<u32, String> {
    let cwd = inside(root, rel)?;
    let pair = native_pty_system().openpty(PtySize { rows: rows.max(5), cols: cols.max(20), pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
    let mut cmd = shell(); cmd.cwd(&cwd); cmd.env("TERM", "xterm-256color");
    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let id = { let mut n = terms.next.lock().unwrap(); *n += 1; *n };
    terms.map.lock().unwrap().insert(id, Term { writer, master: pair.master, child });
    let (app2, terms2) = (app.clone(), terms.clone());
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop { match reader.read(&mut buf) { Ok(0) | Err(_) => break, Ok(n) => { let _ = app2.emit_to("tools", "term", json!({ "id": id, "data": String::from_utf8_lossy(&buf[..n]) })); } } }
        terms2.map.lock().unwrap().remove(&id);
        let _ = app2.emit_to("tools", "term", json!({ "id": id, "exit": true }));
    });
    Ok(id)
}
pub fn term_write(terms: &Terms, id: u32, data: &str) -> Result<(), String> { let mut m = terms.map.lock().unwrap(); let t = m.get_mut(&id).ok_or("That terminal is closed.")?; t.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?; t.writer.flush().map_err(|e| e.to_string()) }
pub fn term_resize(terms: &Terms, id: u32, cols: u16, rows: u16) -> Result<(), String> { let m = terms.map.lock().unwrap(); let t = m.get(&id).ok_or("That terminal is closed.")?; t.master.resize(PtySize { rows: rows.max(5), cols: cols.max(20), pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string()) }
pub fn term_close(terms: &Terms, id: u32) { if let Some(mut t) = terms.map.lock().unwrap().remove(&id) { let _ = t.child.kill(); } }

// ---------- the build / run runner ----------
#[derive(Default)]
pub struct Runs { pub map: Mutex<HashMap<u32, Arc<Mutex<std::process::Child>>>>, next: Mutex<u32> }
/// Runs one command line in a project folder (through the system shell, as typed in the build list); its output
/// arrives as "run" events { id, line, err } and { id, done, code }.
pub fn run_start(app: &AppHandle, runs: &Arc<Runs>, root: &str, rel: &str, cmdline: &str) -> Result<u32, String> {
    let cwd = inside(root, rel)?;
    if cmdline.trim().is_empty() { return Err("Nothing to run.".into()); }
    let mut c = if cfg!(windows) { let mut c = std::process::Command::new("cmd"); c.args(["/d", "/s", "/c", cmdline]); c } else { let mut c = std::process::Command::new("sh"); c.args(["-c", cmdline]); c };
    c.current_dir(&cwd).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    let (out, err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let id = { let mut n = runs.next.lock().unwrap(); *n += 1; *n };
    let child = Arc::new(Mutex::new(child));
    runs.map.lock().unwrap().insert(id, child.clone());
    for (stream, is_err) in [(Box::new(out) as Box<dyn Read + Send>, false), (Box::new(err) as Box<dyn Read + Send>, true)] {
        let app2 = app.clone();
        std::thread::spawn(move || { use std::io::BufRead; for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) { let _ = app2.emit_to("tools", "run", json!({ "id": id, "line": line, "err": is_err })); } });
    }
    let (app3, runs3) = (app.clone(), runs.clone());
    std::thread::spawn(move || {
        let code = loop { if let Ok(Some(s)) = child.lock().unwrap().try_wait() { break s.code().unwrap_or(-1); } std::thread::sleep(std::time::Duration::from_millis(150)); };
        runs3.map.lock().unwrap().remove(&id);
        let _ = app3.emit_to("tools", "run", json!({ "id": id, "done": true, "code": code }));
    });
    Ok(id)
}
pub fn run_stop(runs: &Runs, id: u32) { if let Some(c) = runs.map.lock().unwrap().get(&id) { let _ = c.lock().unwrap().kill(); } }

// ---------- git, with Forgejo sign-in ----------
/// Runs git in a project folder. For the Forgejo host only, the person's token goes along as an HTTP header for this
/// one call (never written into the repo's config, never on the command line of a long-lived process).
pub fn git(root: &str, rel: &str, args: &[String], forgejo_url: &str, token: &str) -> Result<Value, String> {
    let cwd = inside(root, rel)?;
    let allowed = ["status", "log", "diff", "pull", "push", "fetch", "add", "commit", "checkout", "branch", "clone", "remote", "switch", "stash", "restore", "init", "merge", "rebase", "show", "tag", "rev-parse"];
    let Some(sub) = args.first() else { return Err("Which git command?".into()) };
    if !allowed.contains(&sub.as_str()) { return Err(format!("git {sub} is not offered here; use the terminal for it.")); }
    let mut c = std::process::Command::new("git");
    if !token.is_empty() && forgejo_url.starts_with("https://") {
        let base = forgejo_url.trim_end_matches('/');
        c.arg("-c").arg(format!("http.{base}/.extraHeader=Authorization: token {token}"));
    }
    if sub == "clone" { c.args(["-c", "protocol.ext.allow=never"]); }
    c.args(args).current_dir(&cwd).env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    let o = c.output().map_err(|e| format!("git is not installed or not on the PATH ({e})"))?;
    Ok(json!({ "ok": o.status.success(), "code": o.status.code(), "out": String::from_utf8_lossy(&o.stdout), "err": String::from_utf8_lossy(&o.stderr).replace(token, "•••") }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn never_above_the_folder() {
        assert!(super::inside("/p", "../etc").is_err());
        assert!(super::inside("/p", "a/../../b").is_err());
        assert_eq!(super::inside("/p", "a/./b").unwrap(), std::path::PathBuf::from("/p/a/b"));
        assert!(super::inside("", "a").is_err());
    }
}
