//! The heart & knife control app's commands (bridge/vr-host.js): status, start / stop / restart / pause / resume /
//! update, the log, every setting, locking the config down, and start at boot:
//!   Windows: the VibeRollerAIHost service (from boot, before anyone signs in; administrator once) / at sign-in (a
//!            background start, no window; no administrator) / off
//!   Linux:   the system service (sudo once) / a user service (no sudo) / off
use serde_json::Value;
use std::path::PathBuf;

use crate::files::Folder;
use crate::proc::run;
use vr_core::settings::HostSettings;

pub const WIN: bool = cfg!(windows);
pub const SVC: &str = if WIN { "VibeRollerAIHost" } else { "viberoller-ai-host" };

pub fn exe() -> String { std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "vr-host".into()) }
fn home() -> PathBuf { pgm_platform_internal::folders::home() }

// The service, the start at sign-in, the systemd units: pgm-platform-internal (PGM_Platform_Internal) since 5.0.0; what
// the VibeRoller AI Host is as a service is said here.
pub use pgm_platform_internal::service::Service;
pub fn service() -> Service { pgm_platform_internal::service::status(SVC) }
fn names() -> pgm_platform_internal::service::ServiceSpec { pgm_platform_internal::service::ServiceSpec { name: SVC.into(), display: "VibeRoller AI Host".into(), ..Default::default() } }
pub fn boot_mode(s: &Service) -> &'static str { pgm_platform_internal::service::boot_mode(&names(), s) }
pub fn boot_label(m: &str) -> String { pgm_platform_internal::service::boot_label(&names(), m) }
pub fn boot_modes() -> &'static [&'static str] { pgm_platform_internal::service::boot_modes() }

/// The command line the service / the background start / the user unit run.
fn run_line(f: &Folder) -> (String, Vec<String>) { (exe(), vec!["run".into(), "--config".into(), f.config.display().to_string()]) }
/// The VibeRoller AI Host as a service.
fn spec(f: &Folder) -> pgm_platform_internal::service::ServiceSpec {
    let (exe, args) = run_line(f);
    pgm_platform_internal::service::ServiceSpec {
        name: SVC.into(), display: "VibeRoller AI Host".into(),
        description: "Lends this PC's AI (its Claude / ChatGPT login, local models, Unity's engines) to the VibeRoller hub, never the computer itself. Starts at boot.".into(),
        // the service runs as LocalSystem; the logins it lends are this person's (their profile folder is passed on)
        service_bin: format!("\"{exe}\" service-run --config \"{}\" --home \"{}\"", f.config.display(), home().display()),
        signin_exe: hostw(&exe), elevate_args: format!("boot {{mode}} --config \"{}\"", f.config.display()),
        exe, args, dir: f.dir.clone(), log: f.log(), log_name: "ai-node.log".into(),
    }
}
pub fn set_boot(f: &Folder, mode: &str) -> Vec<String> { pgm_platform_internal::service::set_boot(&spec(f), mode) }
/// The window-less starter next to this program on Windows (vr-hostw.exe), else this program.
pub fn hostw(exe: &str) -> String {
    let p = PathBuf::from(exe).with_file_name(if WIN { "vr-hostw.exe" } else { "vr-hostw" });
    if p.exists() { p.display().to_string() } else { exe.to_string() }
}

pub fn start(f: &Folder) -> String {
    let _ = std::fs::remove_file(f.off());
    let svc = service();
    if svc.exists {
        let (ok, out) = pgm_platform_internal::service::start(SVC, &svc);
        return if ok || out.contains("1056") || out.to_lowercase().contains("already") { "Started (the service runs it).".into() } else { format!("Could not start the service: {}", out.trim().lines().last().unwrap_or("")) };
    }
    if let Some(pid) = f.host_pid() { return format!("It is running already (pid {pid})."); }
    if !f.config.exists() { return "No ai-node.json here yet: set the hub, id and token first ([c] settings), or download your own host's package.".into(); }
    match spawn_background(f) { Ok(()) => "Started (in the background; its log is ai-node.log).".into(), Err(e) => format!("Could not start it: {e}") }
}
/// `vr-host run` in the background, its output in ai-node.log, no window.
pub fn spawn_background(f: &Folder) -> std::io::Result<()> { let (exe, args) = run_line(f); pgm_platform_internal::proc::spawn_detached(&exe, &args, &f.dir, &f.log()) }
pub fn stop(f: &Folder, now: bool) -> String {
    let svc = service();
    if svc.exists && svc.running {
        let (ok, out) = pgm_platform_internal::service::stop(SVC, &svc);
        return if ok { "Stopped (the service). VibeRoller keeps running for everyone; Unity says her AI host is offline.".into() } else { format!("Could not stop the service: {}", out.trim().lines().last().unwrap_or("")) };
    }
    if f.host_pid().is_none() { return "It is not running.".into(); }
    let _ = std::fs::write(f.stop_now(), if now { "now" } else { "quiet" });
    if now { "Stopping now.".into() } else { "Stopping as soon as nothing is in flight (it never cuts off an answer).".into() }
}
pub fn restart(f: &Folder, now: bool) -> String {
    if f.host_pid().is_none() { return start(f); }
    let _ = std::fs::write(f.restart_now(), if now { "now" } else { "quiet" });
    if now { "Restarting now.".into() } else { "Restarting as soon as nothing is in flight.".into() }
}
pub fn pause(f: &Folder) -> String { let _ = std::fs::write(f.off(), format!("AI paused from the control app at {}\n", vr_core::now_ms())); "Paused: the host stays up but lends nothing (the hub sees it offline). Resume with [p].".into() }
pub fn resume(f: &Folder) -> String { let _ = std::fs::remove_file(f.off()); "Resumed.".into() }
pub fn update_now(f: &Folder) -> String {
    if f.host_pid().is_none() { return "It is not running: start it, and it looks for an update right after it connects.".into(); }
    let _ = std::fs::write(f.update_now(), vr_core::now_ms().to_string());
    "Asked it to look for an update now (applied as soon as nothing is in flight).".into()
}
pub fn log_tail(f: &Folder, n: usize) -> Vec<String> {
    if let Ok(t) = std::fs::read_to_string(f.log()) { let l: Vec<String> = t.lines().filter(|l| !l.is_empty()).map(String::from).collect(); return l[l.len().saturating_sub(n)..].to_vec(); }
    if !WIN { let s = service(); if s.exists { let n = n.to_string(); let mut a = vec![]; if s.scope == "user" { a.push("--user"); } a.extend(["-u", SVC, "-n", &n, "--no-pager"]); return run("journalctl", &a).1.lines().map(String::from).collect(); } }
    vec!["(no log file here: started by hand, it logs to its own window)".into()]
}
pub fn lockdown(f: &Folder) -> String {
    if !f.config.exists() { return "No ai-node.json here yet.".into(); }
    if !WIN { vr_core::settings::private(&f.config); return "ai-node.json: readable by you only.".into(); }
    #[cfg(windows)]
    { match vr_core::settings::win_lock(&f.config) { Ok(()) => "ai-node.json: only you, SYSTEM (the service) and Administrators may read it.".into(), Err(e) => format!("Could not change its permissions: {e}") } }
    #[cfg(not(windows))]
    { String::new() }
}
/// Is the config locked down? (Linux: no group / other bits; Windows: no "Users" / "Everyone" entry in its ACL.)
pub fn config_private(f: &Folder) -> Option<bool> {
    if !f.config.exists() { return None; }
    Some(!vr_core::settings::readable_by_others(&f.config))
}

/// Everything the dashboard shows, in one read.
pub struct Snapshot { pub cfg: Option<HostSettings>, pub st: Value, pub pid: Option<u32>, pub svc: Service, pub live: bool, pub connected: bool, pub paused: bool, pub boot: &'static str, pub version: String }
pub fn snapshot(f: &Folder) -> Snapshot {
    let cfg = HostSettings::load(&f.config).ok();
    let st = f.status_json();
    let pid = f.host_pid();
    let svc = service();
    let live = pid.is_some() && st["pid"].as_u64() == pid.map(|p| p as u64);
    let connected = live && st["connected"].as_bool() == Some(true) && vr_core::now_ms().saturating_sub(st["lastOkAt"].as_u64().unwrap_or(0)) < 90_000;
    let boot = boot_mode(&svc);
    // the version of the program in that folder: what its running host says, else this program's own when it is that
    // program, else VERSION.txt (a download's note, which an update in place does not rewrite: it said 5.6.0 after 5.7.1)
    let same_exe = std::env::current_exe().ok().and_then(|e| std::fs::canonicalize(e).ok()) == std::fs::canonicalize(f.f(crate::install::exe_name())).ok();
    let version = st["version"].as_str().filter(|v| live && !v.is_empty()).map(String::from)
        .or_else(|| same_exe.then(|| crate::runner::VERSION.to_string()))
        .or_else(|| std::fs::read_to_string(f.f("VERSION.txt")).ok().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| crate::runner::VERSION.to_string());
    Snapshot { paused: f.off().exists(), cfg, st, pid, svc, live, connected, boot, version }
}
pub fn ago(t: u64) -> String { if t == 0 { return "never".into(); } let s = vr_core::now_ms().saturating_sub(t) / 1000; if s < 60 { format!("{s} s ago") } else if s < 3600 { format!("{} min ago", s / 60) } else { format!("{} h ago", s / 3600) } }
pub fn dur(t: u64) -> String { if t == 0 { return String::new(); } let s = vr_core::now_ms().saturating_sub(t) / 1000; let (h, m) = (s / 3600, (s / 60) % 60); if h > 0 { format!("{h} h {m} min") } else if m > 0 { format!("{m} min") } else { format!("{s} s") } }
/// The dashboard as (label, text, tone) rows: tone "ok" / "warn" / "bad" / "".
pub fn rows(s: &Snapshot, f: &Folder) -> Vec<(String, String, &'static str)> {
    let mut r = vec![];
    let st = &s.st;
    let cfg = s.cfg.as_ref();
    let n = |k: &str| st[k].as_u64().unwrap_or(0);
    r.push(("Host".into(), format!("{}{}", st["nickname"].as_str().map(String::from).or_else(|| cfg.map(|c| c.nickname())).unwrap_or_else(|| "(no ai-node.json yet)".into()), cfg.map(|c| c.user()).filter(|u| !u.is_empty()).map(|u| format!("  · owner {u}")).unwrap_or_default()), ""));
    r.push(("Running".into(), match s.pid { Some(pid) => format!("YES (pid {pid}{}){}{}", if s.live && n("startedAt") > 0 { format!(", up {}", dur(n("startedAt"))) } else { String::new() }, if s.svc.exists { format!(" · service {}", if s.svc.running { "running" } else { "stopped" }) } else { String::new() }, if s.paused { "  PAUSED (the knife)" } else { "" }), None => "NO".into() }, if s.pid.is_some() { if s.paused { "warn" } else { "ok" } } else { "bad" }));
    r.push(("At boot".into(), boot_label(s.boot).into(), ""));
    let hub = cfg.map(|c| c.hub()).unwrap_or_default();
    r.push(("Hub".into(), if s.connected { format!("CONNECTED to {hub} since {} · heartbeat {}{}", dur(n("connectedAt")), ago(n("lastOkAt")), st["hubVersion"].as_str().filter(|v| !v.is_empty()).map(|v| format!(" · hub {v}")).unwrap_or_default()) } else if s.pid.is_some() { format!("NOT CONNECTED{}{} (it keeps reconnecting)", if n("downSince") > 0 { format!(" for {}", dur(n("downSince"))) } else { String::new() }, st["lastError"].as_str().filter(|e| !e.is_empty()).map(|e| format!(" · {e}")).unwrap_or_default()) } else { "—".into() }, if s.connected { "ok" } else if s.pid.is_some() { "bad" } else { "" }));
    r.push(("In use".into(), if s.live { format!("{} · last job {}{}", if n("active") > 0 || n("runs") > 0 { format!("{} answer(s) in flight{}", n("active"), if n("runs") > 0 { format!(", {} Work run(s)", n("runs")) } else { String::new() }) } else { "idle".into() }, ago(n("lastJobAt")), st["pending"].as_str().map(|p| format!("  {p}")).unwrap_or_default()) } else { "—".into() }, if n("active") > 0 { "warn" } else { "" }));
    r.push(("Updates".into(), format!("{} · last: {}", if cfg.map(|c| c.auto_update()).unwrap_or(true) { "automatic" } else { "automatic updates OFF" }, st["lastUpdate"].as_str().filter(|u| !u.is_empty()).map(|u| format!("{u} ({})", ago(n("lastUpdateAt")))).unwrap_or_else(|| "none yet".into())), if cfg.map(|c| c.auto_update()).unwrap_or(true) { "" } else { "warn" }));
    let group: Vec<String> = st["group"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let provs = cfg.map(|c| c.all_providers()).unwrap_or_default();
    r.push(("Sharing".into(), if provs.is_empty() { "no providers".into() } else { provs.iter().map(|p| { let mut who: Vec<String> = vec![]; if p.share_with_group { who.push(if group.is_empty() { "the group".into() } else { format!("the group ({})", group.join(", ")) }); } who.extend(p.allowed_users.clone()); format!("{}{}: {}", p.id, if p.enabled { "" } else { " (off)" }, if who.is_empty() { "owner only".into() } else { who.join(", ") }) }).collect::<Vec<_>>().join(" · ") }, ""));
    let engines: Vec<String> = cfg.map(|c| c.engines().into_iter().map(|(k, _)| k).collect()).unwrap_or_default();
    r.push(("Engines".into(), if engines.is_empty() { "none".into() } else { engines.join(", ") }, ""));
    let mut sec = vec![];
    if let Some(c) = cfg {
        let h = c.hub();
        sec.push(if h.starts_with("https://") { "hub over https".to_string() } else if h.starts_with("http://127.0.0.1") || h.starts_with("http://localhost") { "hub on this machine".into() } else { format!("hub NOT over https{}", if c.allow_insecure_hub() { " (allowed)" } else { " (the host refuses it)" }) });
        sec.push(if c.allow_work() { "Work runs on this machine: ON".into() } else { "Work on this machine: off".into() });
        sec.push(c.pc_folder().map(|p| format!("PC files: {}", p.display())).unwrap_or_else(|| "PC files: off".into()));
        sec.push(if c.engine_guard() { "engine guard on".into() } else { "engine guard off".into() });
        if let Some(p) = config_private(f) { sec.push(if p { "config private".into() } else { "config readable by others".into() }); }
    }
    let risky = sec.iter().any(|s| s.contains("NOT") || s.contains("ON") || s.contains("readable by others") || s.contains("guard off"));
    r.push(("Security".into(), sec.join(" · "), if risky { "warn" } else { "ok" }));
    r
}
