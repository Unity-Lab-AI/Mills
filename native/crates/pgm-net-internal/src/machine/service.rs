//! A linked machine as a persistent service (plans/linked-machines.md 8.1 and 8.2, T-313): ONE template that generates
//! everything an app needs to keep `<app> machine run` going, and the state-folder check every start makes.
//!
//! - Linux: a systemd USER unit (`~/.config/systemd/user/<name>.service`): runs as the person who installed it, never root,
//!   `Restart=on-failure`, no restart after "unlinked" or "refused to start" (exit 3 / 4). `loginctl enable-linger` is offered,
//!   never done without asking.
//! - Windows: a per-user Scheduled Task at logon (no administrator, runs as the person with the least privilege level,
//!   restarts on failure). A real Windows service needs one admin approval and is the AI host's `winservice.rs`; this build
//!   installs only the per-user task, which is the spec's default.
//! - `.sh` and `.cmd` twins of the install and uninstall, written next to the unit / task file so a person can read what is done.
//! - [`check_state_dir`]: the state folder (token, device key, hub key, link record, scope, audit log) must be owner-only. Linux:
//!   no group / other bits on the folder or the secret files. Windows: no Users / Everyone / Authenticated Users entry in the
//!   ACL (`pgm_core_internal::fs::readable_by_others`, the icacls helper Jcode PGM uses too). A machine that finds it is not
//!   refuses to start and says how to fix it.
//! Linux and Windows only.
use std::path::{Path, PathBuf};

/// `machine run` ended because the hub unlinked this machine (systemd must not start it again).
pub const EXIT_REVOKED: i32 = 3;
/// `machine run` refused to start: not linked, or the state folder is not private (systemd must not loop on it).
pub const EXIT_REFUSED: i32 = 4;

/// Files in the state folder that hold a secret or the rules: they may never be group- or world-readable.
const SECRET_FILES: &[&str] = &["token", "device-key", "hub-key", "link.json", "scope.json"];

/// Refuses (Err says why and how to fix it) when the state folder, or a secret file in it, can be read by anyone but its owner.
/// A folder that does not exist yet is fine (it is made private). The audit log files are tightened here (they were made with the
/// default umask by an earlier build) rather than refused: nothing in them is a secret, but they should not be open either.
pub fn check_state_dir(dir: &Path) -> Result<(), String> {
    if !dir.exists() { return Ok(()); }
    if pgm_core_internal::fs::readable_by_others(dir) {
        return Err(format!("the state folder {} can be read by other people on this computer (it holds the token, the device key and the scope), so the machine will not start. Fix: {}", dir.display(), fix_hint(dir)));
    }
    for f in SECRET_FILES {
        let p = dir.join(f);
        if p.is_file() && pgm_core_internal::fs::readable_by_others(&p) {
            return Err(format!("{} can be read by other people on this computer, so the machine will not start. Fix: {}", p.display(), fix_hint(&p)));
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("machine-audit.jsonl") { pgm_core_internal::fs::private(&e.path()); }
        }
    }
    Ok(())
}

fn fix_hint(p: &Path) -> String {
    if cfg!(windows) { format!("icacls \"{}\" /inheritance:r /grant:r \"%USERDOMAIN%\\%USERNAME%\":(OI)(CI)F", p.display()) }
    else { format!("chmod {} '{}'", if p.is_dir() { "700" } else { "600" }, p.display()) }
}

/// Everything that names the program as a service.
#[derive(Clone, Debug)]
pub struct Unit {
    /// the unit / task name, e.g. `vibe-roller-machine`
    pub name: String,
    /// what people read, e.g. `VibeRoller linked machine`
    pub display: String,
    /// the program (absolute path)
    pub exe: PathBuf,
    /// the arguments after the program, without the state folder, e.g. `machine run`
    pub args: Vec<String>,
    pub state_dir: PathBuf,
    /// the account it runs as (shown in the files; the unit itself is a user unit, so the account is whoever installs it)
    pub user: String,
    /// the log file
    pub log: PathBuf,
}

/// What a generated file is, for the writer.
pub struct Generated { pub file: &'static str, pub text: String, pub exec: bool }

fn sd_quote(s: &str) -> String {
    let s = s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$");
    if s.is_empty() || s.contains([' ', '\t', ';', '\'']) { format!("\"{s}\"") } else { s }
}
fn cmd_quote(s: &str) -> String { format!("\"{}\"", s.replace('"', "")) }
fn xml(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;") }
fn sh_quote(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) }

impl Unit {
    pub fn new(name: &str, display: &str, exe: &Path, args: &[&str], state_dir: &Path) -> Unit {
        let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
        Unit { name: name.to_string(), display: display.to_string(), exe: exe.to_path_buf(), args: args.iter().map(|s| s.to_string()).collect(), state_dir: state_dir.to_path_buf(), user, log: state_dir.join("machine.log") }
    }
    fn full_args(&self) -> Vec<String> {
        let mut a = self.args.clone();
        a.push("--state".into());
        a.push(self.state_dir.display().to_string());
        a
    }
    /// The Windows account the task runs as: `DOMAIN\user`.
    pub fn windows_account(&self) -> String {
        match std::env::var("USERDOMAIN") { Ok(d) if !d.is_empty() && !self.user.contains('\\') => format!("{d}\\{}", self.user), _ => self.user.clone() }
    }

    /// The systemd user unit.
    pub fn systemd_unit(&self) -> String {
        let exec = std::iter::once(sd_quote(&self.exe.display().to_string())).chain(self.full_args().iter().map(|a| sd_quote(a))).collect::<Vec<_>>().join(" ");
        format!(
"# {display}: serves this computer to its owner's hub as a linked machine (plans/linked-machines.md).
# A systemd USER unit: it runs as {user}, the person who installed it, and never as root.
[Unit]
Description={display}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={exec}
Restart=on-failure
RestartSec=10
# 3 = unlinked on the web, 4 = refused to start (not linked, or the state folder is not private): do not start it again
RestartPreventExitStatus={revoked} {refused}
NoNewPrivileges=yes
UMask=0077
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
", display = self.display, user = if self.user.is_empty() { "the signed-in user" } else { &self.user }, exec = exec, revoked = EXIT_REVOKED, refused = EXIT_REFUSED, log = self.log.display())
    }

    /// The Windows Scheduled Task (XML): at this person's logon, least privilege, restarted when it fails, no time limit.
    pub fn task_xml(&self) -> String {
        let args = self.full_args().iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ");
        format!(
"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">
  <RegistrationInfo><Description>{display}: serves this computer to its owner's hub as a linked machine. Per-user, no administrator.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{account}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id=\"Author\"><UserId>{account}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure><Interval>PT1M</Interval><Count>99</Count></RestartOnFailure>
    <Hidden>true</Hidden>
  </Settings>
  <Actions Context=\"Author\"><Exec><Command>{exe}</Command><Arguments>{args}</Arguments></Exec></Actions>
</Task>
", display = xml(&self.display), account = xml(&self.windows_account()), exe = xml(&self.exe.display().to_string()), args = xml(&args))
    }

    fn unit_file_name(&self) -> String { format!("{}.service", self.name) }

    /// `install.sh`: the unit into the user's systemd folder and started. Refuses root.
    pub fn install_sh(&self) -> String {
        format!(
"#!/bin/sh
# {display}: install as a systemd USER unit (no root, no sudo). Generated; read it, then run it as yourself.
set -eu
[ \"$(id -u)\" != 0 ] || {{ echo \"Run this as yourself, not as root: the unit belongs to your own account.\"; exit 1; }}
command -v systemctl >/dev/null || {{ echo \"systemctl was not found: this computer has no systemd.\"; exit 1; }}
UNIT_DIR=\"${{XDG_CONFIG_HOME:-$HOME/.config}}/systemd/user\"
mkdir -p \"$UNIT_DIR\"
cat > \"$UNIT_DIR/{unit}\" <<'VR_UNIT_EOF'
{text}VR_UNIT_EOF
mkdir -p {state}; chmod 700 {state}
systemctl --user daemon-reload
systemctl --user enable --now {name}.service
echo \"{display} is installed for $(id -un) and running (journalctl --user -u {name} -f; log: {log}).\"
if command -v loginctl >/dev/null && [ \"$(loginctl show-user \"$(id -un)\" -p Linger --value 2>/dev/null)\" != yes ]; then
  echo \"Without lingering it stops when you sign out. To keep it running after sign-out: loginctl enable-linger $(id -un)\"
  if [ -t 0 ]; then printf 'Turn lingering on now? [y/N] '; read -r a; case \"$a\" in y|Y|yes) loginctl enable-linger \"$(id -un)\";; esac; fi
fi
", display = self.display, unit = self.unit_file_name(), text = self.systemd_unit(), state = sh_quote(&self.state_dir.display().to_string()), name = self.name, log = self.log.display())
    }

    pub fn uninstall_sh(&self) -> String {
        format!(
"#!/bin/sh
# {display}: remove the systemd user unit. The state folder (token, key, scope, audit log) stays: delete {state} to forget the link; unlink the machine on the web too.
set -eu
UNIT_DIR=\"${{XDG_CONFIG_HOME:-$HOME/.config}}/systemd/user\"
systemctl --user disable --now {name}.service 2>/dev/null || true
rm -f \"$UNIT_DIR/{unit}\"
systemctl --user daemon-reload 2>/dev/null || true
echo \"{display} no longer starts by itself.\"
", display = self.display, state = self.state_dir.display(), name = self.name, unit = self.unit_file_name())
    }

    /// `install.cmd`: the per-user task from the XML next to it. No administrator; says so if it is run elevated.
    pub fn install_cmd(&self) -> String {
        let t = format!("{}.task.xml", self.name);
        format!(
"@echo off\r\nrem {display}: install as a per-user Scheduled Task at logon (no administrator). Generated; read it, then run it as yourself.\r\nsetlocal\r\nnet session >nul 2>&1 && echo Note: this is an administrator prompt. The task is made for the account that runs it, with least privilege. Run it from your normal prompt if that is not what you want.\r\nif not exist {state} mkdir {state}\r\nicacls {state} /inheritance:r /grant:r \"%USERDOMAIN%\\%USERNAME%\":(OI)(CI)F >nul\r\nschtasks /Create /TN {task} /XML \"%~dp0{xml}\" /F\r\nif errorlevel 1 (echo Could not create the task. & exit /b 1)\r\nschtasks /Run /TN {task} >nul\r\necho {display} is installed for %USERNAME% and starts at every sign-in. Log: {log}\r\n",
            display = self.display, state = cmd_quote(&self.state_dir.display().to_string()), task = cmd_quote(&self.display), xml = t, log = self.log.display())
    }

    pub fn uninstall_cmd(&self) -> String {
        format!(
"@echo off\r\nrem {display}: remove the Scheduled Task. The state folder (token, key, scope, audit log) stays: delete it to forget the link, and unlink the machine on the web too.\r\nschtasks /End /TN {task} >nul 2>&1\r\nschtasks /Delete /TN {task} /F\r\necho {display} no longer starts by itself.\r\n",
            display = self.display, task = cmd_quote(&self.display))
    }

    /// Every file of the template.
    pub fn generate(&self) -> Vec<Generated> {
        vec![
            Generated { file: "install.sh", text: self.install_sh(), exec: true },
            Generated { file: "uninstall.sh", text: self.uninstall_sh(), exec: true },
            Generated { file: "install.cmd", text: self.install_cmd(), exec: false },
            Generated { file: "uninstall.cmd", text: self.uninstall_cmd(), exec: false },
            Generated { file: "unit.service", text: self.systemd_unit(), exec: false },
        ].into_iter().chain(std::iter::once(Generated { file: "task.xml", text: self.task_xml(), exec: false })).collect()
    }

    /// Writes the scripts (and the unit / task files they use) into `dir`. Returns the paths.
    pub fn write_scripts(&self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(dir)?;
        let mut out = vec![];
        for g in self.generate() {
            // the .cmd twin reads <name>.task.xml
            let file = if g.file == "task.xml" { format!("{}.task.xml", self.name) } else if g.file == "unit.service" { self.unit_file_name() } else { g.file.to_string() };
            let p = dir.join(&file);
            std::fs::write(&p, g.text.as_bytes())?;
            #[cfg(unix)]
            if g.exec { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?; }
            out.push(p);
        }
        Ok(out)
    }

    /// Installs for this person on this system: Linux writes the unit and starts it with `systemctl --user`; Windows registers the
    /// per-user task with `schtasks`. `unit_dir` overrides the systemd folder (tests). `run_system` false = write the files only.
    /// Refuses root on Linux. Returns what was done, line by line.
    pub fn install(&self, scripts_dir: &Path, unit_dir: Option<&Path>, run_system: bool) -> Result<Vec<String>, String> {
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 { return Err("the service belongs to a person's own account: run this as yourself, not as root".into()); }
        check_state_dir(&self.state_dir)?;
        pgm_core_internal::fs::private_dir(&self.state_dir).map_err(|e| e.to_string())?;
        let files = self.write_scripts(scripts_dir).map_err(|e| e.to_string())?;
        let mut lines = vec![format!("wrote {} files to {}", files.len(), scripts_dir.display())];
        if cfg!(windows) {
            if !run_system { return Ok(lines); }
            let xml = scripts_dir.join(format!("{}.task.xml", self.name));
            let r = std::process::Command::new("schtasks").args(["/Create", "/TN", &self.display, "/XML", &xml.display().to_string(), "/F"]).output().map_err(|e| format!("schtasks did not start: {e}"))?;
            if !r.status.success() { return Err(format!("could not create the task: {}", String::from_utf8_lossy(&r.stdout).trim().to_string() + String::from_utf8_lossy(&r.stderr).trim())); }
            let _ = std::process::Command::new("schtasks").args(["/Run", "/TN", &self.display]).output();
            lines.push(format!("{} is a per-user Scheduled Task for {} now: it starts at every sign-in (no administrator)", self.display, self.windows_account()));
        } else {
            let ud = unit_dir.map(Path::to_path_buf).unwrap_or_else(user_unit_dir);
            std::fs::create_dir_all(&ud).map_err(|e| e.to_string())?;
            std::fs::write(ud.join(self.unit_file_name()), self.systemd_unit()).map_err(|e| e.to_string())?;
            lines.push(format!("wrote {}", ud.join(self.unit_file_name()).display()));
            if run_system {
                for a in [vec!["--user", "daemon-reload"], vec!["--user", "enable", "--now", &format!("{}.service", self.name)]] {
                    let r = std::process::Command::new("systemctl").args(&a).output().map_err(|e| format!("systemctl did not start: {e}"))?;
                    if !r.status.success() { return Err(format!("systemctl {} failed: {}", a.join(" "), String::from_utf8_lossy(&r.stderr).trim())); }
                }
                lines.push(format!("{} runs as your user service now. To keep it running after you sign out: loginctl enable-linger {}", self.name, self.user));
            }
        }
        Ok(lines)
    }

    /// Is the unit / task there now? (Linux: the unit file; Windows: `schtasks /Query`.)
    pub fn installed(&self) -> bool {
        if cfg!(windows) { std::process::Command::new("schtasks").args(["/Query", "/TN", &self.display]).output().map(|o| o.status.success()).unwrap_or(false) }
        else { user_unit_dir().join(self.unit_file_name()).exists() }
    }

    /// Removes what [`Unit::install`] made (the state folder stays).
    pub fn uninstall(&self, unit_dir: Option<&Path>, run_system: bool) -> Vec<String> {
        let mut lines = vec![];
        if cfg!(windows) {
            if run_system { let _ = std::process::Command::new("schtasks").args(["/End", "/TN", &self.display]).output(); let _ = std::process::Command::new("schtasks").args(["/Delete", "/TN", &self.display, "/F"]).output(); }
            lines.push(format!("{} no longer starts at sign-in", self.display));
        } else {
            let ud = unit_dir.map(Path::to_path_buf).unwrap_or_else(user_unit_dir);
            if run_system { let _ = std::process::Command::new("systemctl").args(["--user", "disable", "--now", &format!("{}.service", self.name)]).output(); }
            let _ = std::fs::remove_file(ud.join(self.unit_file_name()));
            if run_system { let _ = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).output(); }
            lines.push(format!("{} no longer starts by itself", self.name));
        }
        lines.push(format!("the state folder {} stays (delete it to forget the link; unlink the machine on the web too)", self.state_dir.display()));
        lines
    }
}

/// `~/.config/systemd/user` (or under `XDG_CONFIG_HOME`).
pub fn user_unit_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))).unwrap_or_else(|| PathBuf::from(".config"));
    base.join("systemd").join("user")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> Unit {
        let mut u = Unit::new("vibe-roller-machine", "VibeRoller linked machine", Path::new("/opt/my apps/vr-host"), &["machine", "run"], Path::new("/home/ann/.local/state/vibe-roller/machine"));
        u.user = "ann".into();
        u
    }
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("pgm-svc-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    #[test]
    fn the_systemd_unit_is_a_user_unit_that_restarts_on_failure_and_never_runs_as_root() {
        let t = unit().systemd_unit();
        assert!(t.contains("Restart=on-failure"));
        assert!(t.contains("ExecStart=\"/opt/my apps/vr-host\" machine run --state /home/ann/.local/state/vibe-roller/machine"));
        assert!(t.contains("WantedBy=default.target"), "a user unit, not multi-user.target");
        assert!(!t.contains("User=root") && !t.contains("multi-user.target") && !t.contains("\nUser="), "{t}");
        assert!(t.contains("NoNewPrivileges=yes") && t.contains("UMask=0077"));
        assert!(t.contains("RestartPreventExitStatus=3 4"));
        assert!(t.contains("runs as ann"));
    }

    #[test]
    fn the_windows_task_is_per_user_least_privilege_and_restarts() {
        let mut u = unit();
        u.exe = PathBuf::from("C:\\Program Files\\VR\\vr-host.exe");
        u.state_dir = PathBuf::from("C:\\Users\\ann\\AppData\\Local\\VibeRoller\\machine");
        let t = u.task_xml();
        assert!(t.contains("<LogonTrigger>") && t.contains("<RunLevel>LeastPrivilege</RunLevel>") && t.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(t.contains("<RestartOnFailure>") && t.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(!t.contains("HighestAvailable") && !t.contains("LocalSystem") && !t.contains("S-1-5-18"), "no admin, no SYSTEM");
        assert!(t.contains("<Command>C:\\Program Files\\VR\\vr-host.exe</Command>") && t.contains("machine run --state C:\\Users\\ann\\AppData\\Local\\VibeRoller\\machine"));
        assert!(t.contains("ann"));
        let c = u.install_cmd();
        assert!(c.contains("schtasks /Create") && c.contains("/XML") && c.contains("vibe-roller-machine.task.xml") && c.contains("\r\n"));
        assert!(!c.contains("sc create") && !c.contains("LocalSystem"));
        assert!(u.uninstall_cmd().contains("schtasks /Delete"));
    }

    #[test]
    fn sh_and_cmd_twins_exist_and_the_sh_refuses_root() {
        let d = tmp("twins");
        let u = unit();
        let files = u.write_scripts(&d).unwrap();
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        for n in ["install.sh", "uninstall.sh", "install.cmd", "uninstall.cmd", "vibe-roller-machine.service", "vibe-roller-machine.task.xml"] { assert!(names.iter().any(|x| x == n), "{n} in {names:?}"); }
        let sh = std::fs::read_to_string(d.join("install.sh")).unwrap();
        assert!(sh.starts_with("#!/bin/sh") && sh.contains("id -u") && sh.contains("systemctl --user enable --now") && sh.contains("enable-linger"));
        assert!(!sh.contains("sudo ") || sh.contains("not as root"), "no sudo");
        assert!(sh.contains("Restart=on-failure"), "the unit text is inside the script");
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(d.join("install.sh")).unwrap().permissions().mode() & 0o111, 0o111); }
        // the shell script parses
        #[cfg(unix)]
        for f in ["install.sh", "uninstall.sh"] { let r = std::process::Command::new("sh").arg("-n").arg(d.join(f)).output().unwrap(); assert!(r.status.success(), "{f}: {}", String::from_utf8_lossy(&r.stderr)); }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn install_writes_the_unit_and_uninstall_removes_it() {
        if unsafe { libc::geteuid() } == 0 { return; }
        let d = tmp("inst");
        let mut u = unit();
        u.state_dir = d.join("state");
        let ud = d.join("units");
        let lines = u.install(&d.join("scripts"), Some(&ud), false).unwrap();
        assert!(lines.iter().any(|l| l.contains("wrote")));
        assert!(ud.join("vibe-roller-machine.service").exists());
        assert!(d.join("scripts/install.cmd").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&u.state_dir).unwrap().permissions().mode() & 0o777, 0o700);
        let l = u.uninstall(Some(&ud), false);
        assert!(!ud.join("vibe-roller-machine.service").exists());
        assert!(l.iter().any(|x| x.contains("stays")));
        assert!(u.state_dir.exists(), "uninstall keeps the state");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_group_or_world_readable_state_folder_or_secret_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("state");
        let set = |p: &Path, m: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        // missing: fine (it will be made private)
        assert!(check_state_dir(&d.join("nope")).is_ok());
        // private folder, private files: fine
        set(&d, 0o700);
        for f in ["token", "device-key", "scope.json", "hub-key", "link.json"] { std::fs::write(d.join(f), "x").unwrap(); set(&d.join(f), 0o600); }
        std::fs::write(d.join("machine-audit.jsonl"), "{}").unwrap(); set(&d.join("machine-audit.jsonl"), 0o644);
        assert!(check_state_dir(&d).is_ok());
        assert_eq!(std::fs::metadata(d.join("machine-audit.jsonl")).unwrap().permissions().mode() & 0o777, 0o600, "the audit log is tightened, not refused");
        // world-readable folder, group-readable folder
        for m in [0o755, 0o750, 0o770, 0o705] {
            set(&d, m);
            let e = check_state_dir(&d).unwrap_err();
            assert!(e.contains("will not start") && e.contains("chmod 700"), "{m:o}: {e}");
        }
        set(&d, 0o700);
        // a group-readable or world-readable secret in a private folder
        for f in ["token", "device-key", "scope.json", "hub-key", "link.json"] {
            for m in [0o640, 0o644, 0o604, 0o660] {
                set(&d.join(f), m);
                let e = check_state_dir(&d).unwrap_err();
                assert!(e.contains(f) && e.contains("chmod 600"), "{f} {m:o}: {e}");
            }
            set(&d.join(f), 0o600);
        }
        assert!(check_state_dir(&d).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}
