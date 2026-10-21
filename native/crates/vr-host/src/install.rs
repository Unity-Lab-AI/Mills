//! `vr-host install`: the downloaded program installs (or updates) itself, so a download is installer, host and updater
//! in one. It finds an earlier install (the standard folder, the folder the VibeRollerAIHost service / the systemd unit
//! runs from, C:\VibeRollerAIHost, ~/VibeRollerAIHost; a JavaScript host there too) and puts this version in place,
//! keeping that install's own ai-node.json, logs and settings. With nothing installed it installs into the standard
//! folder (Windows: %LOCALAPPDATA%\VibeRoller\AIHost, Linux: ~/.local/share/viberoller/ai-host). A running host there is
//! asked to stop first and is started again on the new version. A host that runs from a git checkout is left alone.
use std::path::{Path, PathBuf};

use crate::control;
use crate::files::{log, Folder};

fn home() -> PathBuf { std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default() }
pub fn standard() -> PathBuf {
    if cfg!(windows) { std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Local")).join("VibeRoller").join("AIHost") }
    else { home().join(".local").join("share").join("viberoller").join("ai-host") }
}
/// Earlier installs on this machine, best first.
pub fn earlier() -> Vec<PathBuf> {
    let mut c = vec![standard()];
    // the folder the service runs from
    if cfg!(windows) {
        let (_, out) = crate::proc::run("sc", &["qc", control::SVC]);
        if let Some(line) = out.lines().find(|l| l.contains("BINARY_PATH_NAME")) {
            let v = line.split_once(':').map(|x| x.1.trim()).unwrap_or("");
            let exe = v.trim_start_matches('"').split('"').next().unwrap_or("");
            if let Some(d) = Path::new(exe).parent() { c.push(d.to_path_buf()); if let Some(dd) = d.parent() { c.push(dd.to_path_buf()); } }
        }
        c.push(PathBuf::from(r"C:\VibeRollerAIHost"));
    } else {
        for user in [false, true] {
            let mut a = vec![]; if user { a.push("--user"); } a.extend(["show", "-p", "WorkingDirectory", control::SVC]);
            let (_, out) = crate::proc::run("systemctl", &a);
            if let Some(d) = out.trim().strip_prefix("WorkingDirectory=") { if !d.is_empty() { c.push(PathBuf::from(d)); } }
        }
    }
    c.push(home().join("VibeRollerAIHost"));
    let mut seen = vec![];
    c.into_iter().filter(|d| d.join("ai-node.json").exists() || d.join("ai-node.js").exists() || d.join(exe_name()).exists()).filter(|d| { let k = std::fs::canonicalize(d).unwrap_or(d.clone()); if seen.contains(&k) { false } else { seen.push(k); true } }).collect()
}
pub fn exe_name() -> &'static str { if cfg!(windows) { "vr-host.exe" } else { "vr-host" } }
/// A JavaScript host that runs from a VibeRoller git checkout (its bridge/ folder): it updates itself through git.
fn in_git(d: &Path) -> bool { d.join("ai-node.js").exists() && d.ancestors().any(|a| a.join(".git").exists()) }

/// Windows: the earlier JavaScript host's separate "Start / Stop VibeRoller AI Host" shortcuts (Desktop, Start menu)
/// point at programs that are gone once this one takes over: they go (the same list its own build script removes).
fn old_shortcuts() -> usize {
    if !cfg!(windows) { return 0; }
    let menu = std::env::var_os("APPDATA").map(PathBuf::from).map(|a| a.join("Microsoft").join("Windows").join("Start Menu").join("Programs"));
    let mut n = 0;
    for dir in [Some(home().join("Desktop")), menu].into_iter().flatten() {
        for name in ["Start VibeRoller AI host.lnk", "Stop VibeRoller AI host.lnk", "Start VibeRoller AI Host.lnk", "Stop VibeRoller AI Host.lnk"] {
            if std::fs::remove_file(dir.join(name)).is_ok() { n += 1; }
        }
    }
    n
}
pub fn install(args: &[&str]) -> i32 {
    let me = std::env::current_exe().unwrap_or_default();
    let here = me.parent().map(Path::to_path_buf).unwrap_or_default();
    let target = earlier().into_iter().find(|d| !in_git(d)).unwrap_or_else(standard);
    if in_git(&target) { println!("The AI host at {} runs from a git checkout: it updates itself from there; nothing done.", target.display()); return 0; }
    let same = std::fs::canonicalize(&target).ok() == std::fs::canonicalize(&here).ok();
    if same { println!("This is the installed AI host ({}).", target.display()); return 0; }
    let f = Folder { dir: target.clone(), config: target.join("ai-node.json") };
    let was_running = f.host_pid().is_some() || control::service().running;
    if let Some(pid) = f.host_pid() {
        println!("An AI host runs there (pid {pid}): asking it to stop (it finishes what is in flight first) …");
        let _ = std::fs::write(f.stop_now(), "quiet");
        for _ in 0..600 { if f.host_pid().is_none() { break; } std::thread::sleep(std::time::Duration::from_millis(200)); }
    }
    if let Err(e) = std::fs::create_dir_all(&target) { println!("Could not make {}: {e}", target.display()); return 1; }
    // the program files; its own settings and logs stay
    for name in [exe_name(), if cfg!(windows) { "vr-hostw.exe" } else { "vr-hostw" }, "README.txt", "VERSION.txt"] {
        let src = here.join(name); if !src.exists() { continue; }
        let dst = target.join(name);
        if dst.exists() && name.starts_with("vr-host") { let old = target.join(format!("{name}.old")); let _ = std::fs::remove_file(&old); let _ = std::fs::rename(&dst, &old); }
        if let Err(e) = std::fs::copy(&src, &dst) { println!("Could not copy {name}: {e}"); return 1; }
    }
    // the settings: this download's own (a host's package) only where the install has none yet
    let cfg_src = here.join("ai-node.json");
    if cfg_src.exists() && !target.join("ai-node.json").exists() { let _ = std::fs::copy(&cfg_src, target.join("ai-node.json")); vr_core::settings::private(&target.join("ai-node.json")); }
    let fresh = !f.config.exists();
    let gone = old_shortcuts();
    if gone > 0 { println!("Removed {gone} shortcut(s) of the earlier JavaScript host's separate start / stop programs."); }
    log(format!("installed VibeRoller AI Host (native) {} into {}", crate::runner::VERSION, target.display()));
    println!("Installed into {}{}.", target.display(), if fresh { " (no ai-node.json yet: VibeRoller → Settings → My AI hosts makes one; or `vr-host set hub …`)" } else { "" });
    // start at boot, when asked (--boot service | sign-in | user | off)
    if let Some(i) = args.iter().position(|a| *a == "--boot") { if let Some(m) = args.get(i + 1) { for l in control::set_boot(&f, m) { println!("{l}"); } } }
    else if control::service().exists { for l in control::set_boot(&f, "service") { println!("{l}"); } } // the service now runs this version
    if was_running && !control::service().running && !fresh { println!("{}", control::start(&f)); }
    0
}
