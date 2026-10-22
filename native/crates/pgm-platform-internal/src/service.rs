//! A program as a service, the same way on both systems:
//!   Windows: a Windows service (from boot, before anyone signs in; administrator once) / at sign-in (a background
//!            start from the Run key, no window; no administrator) / off
//!   Linux:   the systemd system service (sudo once) / a systemd user service (no sudo) / off
//! Everything that names the program comes from a [`ServiceSpec`]; the texts it gives back are for people.
use std::path::PathBuf;

use crate::folders::home;
use crate::proc::run;
use crate::WIN;

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// What a program is as a service.
#[derive(Clone, Debug, Default)]
pub struct ServiceSpec {
    /// the Windows service name / the systemd unit name (without .service)
    pub name: String,
    /// the name people see (Windows DisplayName, the Run key value, the systemd Description)
    pub display: String,
    /// the Windows service's description
    pub description: String,
    /// the program and its arguments for a background start and for the systemd units
    pub exe: String,
    pub args: Vec<String>,
    /// Windows: the full binPath of the service (the program with its service arguments, quoted)
    pub service_bin: String,
    /// Windows: the program the Run key starts at sign-in (a window-less starter; else `exe`)
    pub signin_exe: String,
    /// Windows: the arguments this program takes to do `set_boot(mode)` itself with administrator rights ("{mode}" is
    /// replaced with the mode)
    pub elevate_args: String,
    /// the working folder and the log file the units / the background start write to (and the log's name for texts)
    pub dir: PathBuf,
    pub log: PathBuf,
    pub log_name: String,
}

#[derive(Default, Clone, Debug)]
pub struct Service { pub exists: bool, pub running: bool, pub auto: bool, pub scope: String }

fn user_unit(name: &str) -> PathBuf { home().join(".config").join("systemd").join("user").join(format!("{name}.service")) }

/// Is it installed as a service, running, started at boot? (Linux: the system unit first, then the user unit.)
pub fn status(name: &str) -> Service {
    if WIN {
        let (ok, out) = run("sc", &["query", name]);
        if !ok { return Service::default(); }
        return Service { exists: true, running: out.contains("RUNNING"), auto: run("sc", &["qc", name]).1.contains("AUTO_START"), scope: "service".into() };
    }
    let unit = format!("{name}.service");
    for user in [false, true] {
        let a = |k: &str| { let mut v: Vec<&str> = vec![]; if user { v.push("--user"); } v.push(k); v.push(name); run("systemctl", &v) };
        let mut cat: Vec<&str> = vec![]; if user { cat.push("--user"); } cat.extend(["cat", unit.as_str()]);
        if run("systemctl", &cat).0 {
            return Service { exists: true, running: a("is-active").1.trim() == "active", auto: a("is-enabled").1.trim() == "enabled", scope: if user { "user".into() } else { "system".into() } };
        }
    }
    Service::default()
}
/// How it starts now: "service", "user" (Linux), "sign-in" (Windows) or "off".
pub fn boot_mode(spec: &ServiceSpec, s: &Service) -> &'static str {
    if s.exists && s.auto { return if !WIN && s.scope == "user" { "user" } else { "service" }; }
    if WIN && run("reg", &["query", RUN_KEY, "/v", &spec.display]).0 { return "sign-in"; }
    "off"
}
/// The ways it can start on this system.
pub fn boot_modes() -> &'static [&'static str] { if WIN { &["service", "sign-in", "off"] } else { &["service", "user", "off"] } }
/// A mode in words ("service" on Windows names the service).
pub fn boot_label(spec: &ServiceSpec, m: &str) -> String {
    match m {
        "service" if WIN => format!("the {} service (from boot, before anyone signs in)", spec.name),
        "service" => "the system service (from boot)".into(),
        "user" => "a user service (when you are signed in; from boot with lingering on)".into(),
        "sign-in" => "at sign-in (in the background, no window)".into(),
        _ => "not at boot (start it by hand)".into(),
    }
}
fn quoted(args: &[String]) -> String { args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ") }

/// Sets how it starts: "service", "sign-in" (Windows), "user" (Linux) or "off". What happened, line by line.
pub fn set_boot(spec: &ServiceSpec, mode: &str) -> Vec<String> {
    let mut lines = vec![];
    let svc = status(&spec.name);
    let name = spec.name.as_str();
    if WIN {
        if mode == "service" {
            #[cfg(windows)]
            if !crate::proc::is_admin() {
                lines.push("Windows asks for administrator rights once to install the service …".into());
                if !crate::proc::run_elevated(&spec.exe, &spec.elevate_args.replace("{mode}", mode)) { lines.push("It was not allowed.".into()); }
                return lines;
            }
            let _ = run("reg", &["delete", RUN_KEY, "/v", &spec.display, "/f"]);
            if svc.exists { remove_windows_service(name); }
            let (ok, out) = run("sc", &["create", name, "binPath=", &spec.service_bin, "start=", "auto", "DisplayName=", &spec.display, "obj=", "LocalSystem"]);
            if !ok { lines.push(format!("Could not create the service: {}", out.trim().lines().last().unwrap_or(""))); return lines; }
            let _ = run("sc", &["description", name, &spec.description]);
            let _ = run("sc", &["failure", name, "reset=", "86400", "actions=", "restart/5000/restart/10000/restart/30000"]);
            // signed-in users (not only administrators) may start and stop it (the access list only: an audit part would
            // need a privilege an elevated prompt does not have switched on)
            let _ = run("sc", &["sdset", name, "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPWPDTLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)"]);
            let (ok, _) = run("sc", &["start", name]);
            lines.push(if ok { format!("The {name} service is installed, running, and starts at every boot. Log: {}", spec.log_name) } else { format!("The service is installed; it starts at the next boot (or: sc start {name}).") });
        } else {
            if svc.exists {
                #[cfg(windows)]
                if !crate::proc::is_admin() { lines.push("Windows asks for administrator rights once to remove the service …".into()); let _ = crate::proc::run_elevated(&spec.exe, &spec.elevate_args.replace("{mode}", mode)); return lines; }
                remove_windows_service(name); lines.push("The service is removed.".into());
            }
            if mode == "sign-in" {
                let cmd = format!("\"{}\" {}", spec.signin_exe, quoted(&spec.args));
                let (ok, out) = run("reg", &["add", RUN_KEY, "/v", &spec.display, "/t", "REG_SZ", "/d", &cmd, "/f"]);
                lines.push(if ok { "It starts at sign-in now, in the background (no window).".into() } else { format!("Could not set it: {}", out.trim()) });
            } else { let _ = run("reg", &["delete", RUN_KEY, "/v", &spec.display, "/f"]); lines.push("It no longer starts by itself; start it here when you want it.".into()); }
        }
        return lines;
    }
    let exec = format!("{} {}", spec.exe, quoted(&spec.args));
    let unit = |desc: &str, extra: &str, wanted: &str| format!("[Unit]\nDescription={desc}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nWorkingDirectory={}\nExecStart={exec}\n{extra}Restart=always\nRestartSec=5\nStandardOutput=append:{}\nStandardError=append:{}\n\n[Install]\nWantedBy={wanted}\n", spec.dir.display(), spec.log.display(), spec.log.display());
    if mode == "service" {
        if svc.exists && svc.scope == "user" { let _ = run("systemctl", &["--user", "disable", "--now", name]); let _ = std::fs::remove_file(user_unit(name)); }
        let me = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
        let tmp = std::env::temp_dir().join(format!("{name}.service"));
        let _ = std::fs::write(&tmp, unit(&spec.display, &format!("User={me}\nEnvironment=HOME={}\n", home().display()), "multi-user.target"));
        lines.push("Installing the system service (sudo asks for your password) …".into());
        let ok = std::process::Command::new("sudo").args(["sh", "-c", &format!("install -m 644 '{}' /etc/systemd/system/{name}.service && systemctl daemon-reload && systemctl enable --now {name}", tmp.display())]).status().map(|s| s.success()).unwrap_or(false);
        let _ = std::fs::remove_file(&tmp);
        lines.push(if ok { "The system service is installed, running, and starts at every boot.".into() } else { "Could not install the system service (sudo was refused or failed).".into() });
    } else {
        if svc.exists && svc.scope == "system" {
            lines.push("Removing the system service (sudo asks for your password) …".into());
            let _ = std::process::Command::new("sudo").args(["sh", "-c", &format!("systemctl disable --now {name}; rm -f /etc/systemd/system/{name}.service; systemctl daemon-reload")]).status();
        }
        if mode == "user" {
            let _ = std::fs::create_dir_all(user_unit(name).parent().unwrap_or(&home()));
            let _ = std::fs::write(user_unit(name), unit(&format!("{} (user service)", spec.display), "", "default.target"));
            let _ = run("systemctl", &["--user", "daemon-reload"]);
            let (ok, out) = run("systemctl", &["--user", "enable", "--now", name]);
            let me = std::env::var("USER").unwrap_or_default();
            lines.push(if ok { format!("It runs as your user service now, and starts when you sign in. To start it at boot without signing in: sudo loginctl enable-linger {me}") } else { format!("Could not enable the user service: {}", out.trim().lines().last().unwrap_or("")) });
        } else {
            if svc.exists && svc.scope == "user" { let _ = run("systemctl", &["--user", "disable", "--now", name]); }
            let _ = std::fs::remove_file(user_unit(name));
            lines.push("It no longer starts by itself; start it here when you want it.".into());
        }
    }
    lines
}
/// Windows: stops the service, waits until it has stopped (up to 20 s), deletes it and waits until it is gone: a
/// service only marked for deletion cannot be made again (error 1072).
pub fn remove_windows_service(name: &str) {
    let wait = |done: &dyn Fn() -> bool, secs: u64| { for _ in 0..secs * 4 { if done() { return; } std::thread::sleep(std::time::Duration::from_millis(250)); } };
    let _ = run("sc", &["stop", name]);
    wait(&|| { let (ok, out) = run("sc", &["query", name]); !ok || out.contains("STOPPED") }, 20);
    let _ = run("sc", &["delete", name]);
    wait(&|| !run("sc", &["query", name]).0, 15);
}
/// Starts / stops the installed service: (worked, what the system said).
pub fn start(name: &str, s: &Service) -> (bool, String) { if WIN { run("sc", &["start", name]) } else if s.scope == "user" { run("systemctl", &["--user", "start", name]) } else { run("systemctl", &["start", name]) } }
pub fn stop(name: &str, s: &Service) -> (bool, String) { if WIN { run("sc", &["stop", name]) } else if s.scope == "user" { run("systemctl", &["--user", "stop", name]) } else { run("systemctl", &["stop", name]) } }
