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
fn home() -> PathBuf { std::env::var_os(if WIN { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default() }
fn user_unit() -> PathBuf { home().join(".config").join("systemd").join("user").join(format!("{SVC}.service")) }
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[derive(Default, Clone)]
pub struct Service { pub exists: bool, pub running: bool, pub auto: bool, pub scope: String }
pub fn service() -> Service {
    if WIN {
        let (ok, out) = run("sc", &["query", SVC]);
        if !ok { return Service::default(); }
        return Service { exists: true, running: out.contains("RUNNING"), auto: run("sc", &["qc", SVC]).1.contains("AUTO_START"), scope: "service".into() };
    }
    for user in [false, true] {
        let a = |k: &str| { let mut v: Vec<&str> = vec![]; if user { v.push("--user"); } v.push(k); v.push(SVC); run("systemctl", &v) };
        let mut cat: Vec<&str> = vec![]; if user { cat.push("--user"); } cat.extend(["cat", "viberoller-ai-host.service"]);
        if run("systemctl", &cat).0 {
            return Service { exists: true, running: a("is-active").1.trim() == "active", auto: a("is-enabled").1.trim() == "enabled", scope: if user { "user".into() } else { "system".into() } };
        }
    }
    Service::default()
}
pub fn boot_mode(s: &Service) -> &'static str {
    if s.exists && s.auto { return if !WIN && s.scope == "user" { "user" } else { "service" }; }
    if WIN && run("reg", &["query", RUN_KEY, "/v", "VibeRoller AI Host"]).0 { return "sign-in"; }
    "off"
}
pub fn boot_label(m: &str) -> &'static str {
    match m {
        "service" if WIN => "the VibeRollerAIHost service (from boot, before anyone signs in)",
        "service" => "the system service (from boot)",
        "user" => "a user service (when you are signed in; from boot with lingering on)",
        "sign-in" => "at sign-in (in the background, no window)",
        _ => "not at boot (start it by hand)",
    }
}
pub fn boot_modes() -> &'static [&'static str] { if WIN { &["service", "sign-in", "off"] } else { &["service", "user", "off"] } }

/// The command line the service / the background start / the user unit run.
fn run_line(f: &Folder) -> (String, Vec<String>) { (exe(), vec!["run".into(), "--config".into(), f.config.display().to_string()]) }

pub fn set_boot(f: &Folder, mode: &str) -> Vec<String> {
    let mut lines = vec![];
    let svc = service();
    if WIN {
        let (exe, args) = run_line(f);
        if mode == "service" {
            #[cfg(windows)]
            if !crate::winsys::is_admin() {
                lines.push("Windows asks for administrator rights once to install the service …".into());
                if !crate::winsys::run_elevated(&exe, &format!("boot service --config \"{}\"", f.config.display())) { lines.push("It was not allowed.".into()); }
                return lines;
            }
            let _ = run("reg", &["delete", RUN_KEY, "/v", "VibeRoller AI Host", "/f"]);
            if svc.exists { remove_service(); }
            // the service runs as LocalSystem; the logins it lends are this person's (their profile folder is passed on)
            let bin = format!("\"{exe}\" service-run --config \"{}\" --home \"{}\"", f.config.display(), home().display());
            let (ok, out) = run("sc", &["create", SVC, "binPath=", &bin, "start=", "auto", "DisplayName=", "VibeRoller AI Host", "obj=", "LocalSystem"]);
            if !ok { lines.push(format!("Could not create the service: {}", out.trim().lines().last().unwrap_or(""))); return lines; }
            let _ = run("sc", &["description", SVC, "Lends this PC's AI (its Claude / ChatGPT login, local models, Unity's engines) to the VibeRoller hub, never the computer itself. Starts at boot."]);
            let _ = run("sc", &["failure", SVC, "reset=", "86400", "actions=", "restart/5000/restart/10000/restart/30000"]);
            // signed-in users (not only administrators) may start and stop it: what the heart and the knife do (the access
            // list only: an audit part would need a privilege an elevated prompt does not have switched on)
            let _ = run("sc", &["sdset", SVC, "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPWPDTLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)"]);
            let (ok, _) = run("sc", &["start", SVC]);
            lines.push(if ok { "The VibeRollerAIHost service is installed, running, and starts at every boot. Log: ai-node.log".into() } else { "The service is installed; it starts at the next boot (or: sc start VibeRollerAIHost).".into() });
            let _ = args;
        } else {
            if svc.exists {
                #[cfg(windows)]
                if !crate::winsys::is_admin() { lines.push("Windows asks for administrator rights once to remove the service …".into()); let _ = crate::winsys::run_elevated(&exe, &format!("boot {mode} --config \"{}\"", f.config.display())); return lines; }
                remove_service(); lines.push("The service is removed.".into());
            }
            if mode == "sign-in" {
                let cmd = format!("\"{}\" {}", hostw(&exe), args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" "));
                let (ok, out) = run("reg", &["add", RUN_KEY, "/v", "VibeRoller AI Host", "/t", "REG_SZ", "/d", &cmd, "/f"]);
                lines.push(if ok { "It starts at sign-in now, in the background (no window).".into() } else { format!("Could not set it: {}", out.trim()) });
            } else { let _ = run("reg", &["delete", RUN_KEY, "/v", "VibeRoller AI Host", "/f"]); lines.push("It no longer starts by itself; start it here when you want it.".into()); }
        }
        return lines;
    }
    let (exe, args) = run_line(f);
    let exec = format!("{exe} {}", args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" "));
    let unit = |desc: &str, extra: &str, wanted: &str| format!("[Unit]\nDescription={desc}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nWorkingDirectory={}\nExecStart={exec}\n{extra}Restart=always\nRestartSec=5\nStandardOutput=append:{}\nStandardError=append:{}\n\n[Install]\nWantedBy={wanted}\n", f.dir.display(), f.log().display(), f.log().display());
    if mode == "service" {
        if svc.exists && svc.scope == "user" { let _ = run("systemctl", &["--user", "disable", "--now", SVC]); let _ = std::fs::remove_file(user_unit()); }
        let me = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
        let tmp = std::env::temp_dir().join(format!("{SVC}.service"));
        let _ = std::fs::write(&tmp, unit("VibeRoller AI Host", &format!("User={me}\nEnvironment=HOME={}\n", home().display()), "multi-user.target"));
        lines.push("Installing the system service (sudo asks for your password) …".into());
        let ok = std::process::Command::new("sudo").args(["sh", "-c", &format!("install -m 644 '{}' /etc/systemd/system/{SVC}.service && systemctl daemon-reload && systemctl enable --now {SVC}", tmp.display())]).status().map(|s| s.success()).unwrap_or(false);
        let _ = std::fs::remove_file(&tmp);
        lines.push(if ok { "The system service is installed, running, and starts at every boot.".into() } else { "Could not install the system service (sudo was refused or failed).".into() });
    } else {
        if svc.exists && svc.scope == "system" {
            lines.push("Removing the system service (sudo asks for your password) …".into());
            let _ = std::process::Command::new("sudo").args(["sh", "-c", &format!("systemctl disable --now {SVC}; rm -f /etc/systemd/system/{SVC}.service; systemctl daemon-reload")]).status();
        }
        if mode == "user" {
            let _ = std::fs::create_dir_all(user_unit().parent().unwrap_or(&home()));
            let _ = std::fs::write(user_unit(), unit("VibeRoller AI Host (user service)", "", "default.target"));
            let _ = run("systemctl", &["--user", "daemon-reload"]);
            let (ok, out) = run("systemctl", &["--user", "enable", "--now", SVC]);
            let me = std::env::var("USER").unwrap_or_default();
            lines.push(if ok { format!("It runs as your user service now, and starts when you sign in. To start it at boot without signing in: sudo loginctl enable-linger {me}") } else { format!("Could not enable the user service: {}", out.trim().lines().last().unwrap_or("")) });
        } else {
            if svc.exists && svc.scope == "user" { let _ = run("systemctl", &["--user", "disable", "--now", SVC]); }
            let _ = std::fs::remove_file(user_unit());
            lines.push("It no longer starts by itself; start it here when you want it.".into());
        }
    }
    lines
}
/// Stops the Windows service, waits until it has stopped (the host finishes what is in flight, up to a few seconds),
/// deletes it and waits until it is gone: a service only marked for deletion cannot be made again (error 1072).
fn remove_service() {
    let wait = |done: &dyn Fn() -> bool, secs: u64| { for _ in 0..secs * 4 { if done() { return; } std::thread::sleep(std::time::Duration::from_millis(250)); } };
    let _ = run("sc", &["stop", SVC]);
    wait(&|| { let (ok, out) = run("sc", &["query", SVC]); !ok || out.contains("STOPPED") }, 20);
    let _ = run("sc", &["delete", SVC]);
    wait(&|| !run("sc", &["query", SVC]).0, 15);
}
/// The window-less starter next to this program on Windows (vr-hostw.exe), else this program.
pub fn hostw(exe: &str) -> String {
    let p = PathBuf::from(exe).with_file_name(if WIN { "vr-hostw.exe" } else { "vr-hostw" });
    if p.exists() { p.display().to_string() } else { exe.to_string() }
}

pub fn start(f: &Folder) -> String {
    let _ = std::fs::remove_file(f.off());
    let svc = service();
    if svc.exists {
        let (ok, out) = if WIN { run("sc", &["start", SVC]) } else if svc.scope == "user" { run("systemctl", &["--user", "start", SVC]) } else { run("systemctl", &["start", SVC]) };
        return if ok || out.contains("1056") || out.to_lowercase().contains("already") { "Started (the service runs it).".into() } else { format!("Could not start the service: {}", out.trim().lines().last().unwrap_or("")) };
    }
    if let Some(pid) = f.host_pid() { return format!("It is running already (pid {pid})."); }
    if !f.config.exists() { return "No ai-node.json here yet: set the hub, id and token first ([c] settings), or download your own host's package.".into(); }
    match spawn_background(f) { Ok(()) => "Started (in the background; its log is ai-node.log).".into(), Err(e) => format!("Could not start it: {e}") }
}
/// `vr-host run` in the background, its output in ai-node.log, no window.
pub fn spawn_background(f: &Folder) -> std::io::Result<()> {
    let (exe, args) = run_line(f);
    let log = std::fs::OpenOptions::new().create(true).append(true).open(f.log())?;
    let mut c = std::process::Command::new(&exe);
    c.args(&args).current_dir(&f.dir).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(crate::winsys::CREATE_NO_WINDOW | 0x0000_0008 /* DETACHED_PROCESS */); }
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; c.process_group(0); }
    c.spawn().map(|_| ())
}
pub fn stop(f: &Folder, now: bool) -> String {
    let svc = service();
    if svc.exists && svc.running {
        let (ok, out) = if WIN { run("sc", &["stop", SVC]) } else if svc.scope == "user" { run("systemctl", &["--user", "stop", SVC]) } else { run("systemctl", &["stop", SVC]) };
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
    let version = std::fs::read_to_string(f.f("VERSION.txt")).map(|s| s.trim().to_string()).unwrap_or_else(|_| crate::runner::VERSION.to_string());
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
