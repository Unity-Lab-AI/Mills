//! `vr-host install`: the downloaded program installs (or updates) itself, so a download is installer, host and updater
//! in one. It finds an earlier install (the standard folder, the folder the VibeRollerAIHost service / the systemd unit
//! runs from, C:\VibeRollerAIHost, ~/VibeRollerAIHost; a JavaScript host there too) and puts this version in place,
//! keeping that install's own ai-node.json, logs and settings. With nothing installed it installs into the standard
//! folder (Windows: %LOCALAPPDATA%\VibeRoller\AIHost, Linux: ~/.local/share/viberoller/ai-host). A running host there is
//! asked to stop first and is started again on the new version. A host that runs from a git checkout is left alone.
//! Install also puts `vr-host` on the PATH and, when nothing starts the host at boot yet and a config exists, sets the
//! default start (Linux: a user service, Windows: at sign-in) and starts it.
//!
//! Two environment switches keep tests (and the careful) off the machine: VR_HOST_NO_BOOT=1 leaves systemd / the
//! Windows service and the sign-in entry alone (and does not even read them); VR_HOST_NO_PATH=1 leaves the PATH alone.
use std::path::{Path, PathBuf};

use crate::control;
use crate::files::{log, Folder};

fn home() -> PathBuf { pgm_platform_internal::folders::home() }
pub fn standard() -> PathBuf {
    if cfg!(windows) { std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Local")).join("VibeRoller").join("AIHost") }
    else { home().join(".local").join("share").join("viberoller").join("ai-host") }
}
/// VR_HOST_NO_BOOT is set: the machine's start-at-boot entries are neither read nor written.
pub fn no_boot() -> bool { std::env::var("VR_HOST_NO_BOOT").map(|v| !v.is_empty() && v != "0").unwrap_or(false) }
fn no_path() -> bool { std::env::var_os("VR_HOST_NO_PATH").is_some() || std::env::var_os("VR_LOGIN_HOME").is_some() /* the Windows service runs for someone else's profile */ }
/// Earlier installs on this machine, best first.
pub fn earlier() -> Vec<PathBuf> {
    let mut c = vec![standard()];
    // the folder the service runs from
    if no_boot() {}
    else if cfg!(windows) {
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
fn hostw_name() -> &'static str { if cfg!(windows) { "vr-hostw.exe" } else { "vr-hostw" } }
/// A JavaScript host that runs from a VibeRoller git checkout (its bridge/ folder): it updates itself through git.
fn in_git(d: &Path) -> bool { d.join("ai-node.js").exists() && d.ancestors().any(|a| a.join(".git").exists()) }
fn same_path(a: &Path, b: &Path) -> bool { a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y) }
fn folder_of(d: &Path) -> Folder { Folder { dir: d.to_path_buf(), config: d.join("ai-node.json") } }

/// The install that matters among those found: the one whose host runs (that is the one people use), then the first one
/// found. Two copies on one machine otherwise got the update in the one nobody ran.
pub fn pick_target(found: &[PathBuf]) -> Option<PathBuf> {
    found.iter().find(|d| !in_git(d) && folder_of(d).host_pid().is_some()).or_else(|| found.iter().find(|d| !in_git(d))).cloned()
}

/// The version of the program in a host folder: what its running host says, else its VERSION.txt, else "unknown".
pub fn folder_version(f: &Folder) -> String {
    let st = f.status_json();
    st["version"].as_str().filter(|v| !v.is_empty() && f.host_pid().is_some()).map(String::from)
        .or_else(|| std::fs::read_to_string(f.f("VERSION.txt")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "unknown".into())
}

/// Which copy of the program is this, and is there an installed one?
#[derive(Clone, Debug)]
pub struct Copies {
    pub this_exe: PathBuf,
    /// this program's folder is an installed host folder
    pub this_installed: bool,
    /// the installed host folder that matters (see pick_target), if any
    pub installed: Option<Folder>,
    pub installed_version: String,
    pub installed_running: bool,
}
impl Copies {
    /// The installed program's file (or this one when nothing is installed).
    pub fn installed_exe(&self) -> PathBuf { self.installed.as_ref().map(|f| f.f(exe_name())).filter(|p| p.exists()).unwrap_or_else(|| self.this_exe.clone()) }
}
pub fn copies() -> Copies {
    let this_exe = std::env::current_exe().ok().and_then(|e| std::fs::canonicalize(&e).ok().or(Some(e))).unwrap_or_default();
    let found = earlier();
    let here = this_exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let this_installed = found.iter().any(|d| !in_git(d) && same_path(d, &here));
    let installed = pick_target(&found).map(|d| folder_of(&d));
    let (installed_version, installed_running) = installed.as_ref().map(|f| (folder_version(f), f.host_pid().is_some())).unwrap_or_default();
    Copies { this_exe, this_installed, installed, installed_version, installed_running }
}

/// What `vr-host host` does at every start, from an INSTALLED folder only (this program's own folder is an installed host
/// folder, not a git checkout, not a download): whatever the installed program needs to be complete, so setup follows
/// updates. Hosts update themselves in place and restart through `vr-host host`, so a step added here reaches every
/// machine at its next update: an update that needs more setup adds it here. Idempotent, never fails the caller; returns
/// what it did (nothing when everything was in place). Today: `vr-host` on the PATH, VERSION.txt following the program,
/// the previous version's file removed once the update is confirmed, and a start-at-boot entry that runs another copy
/// is reported.
pub fn ensure_setup(f: &Folder) -> Vec<String> {
    let Ok(exe) = std::env::current_exe() else { return vec![] };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let installed = exe.parent().map(|p| same_path(p, &f.dir) && earlier().iter().any(|d| !in_git(d) && same_path(d, p))).unwrap_or(false);
    if !installed { return vec![]; }
    setup_in(f, &exe, &home())
}
/// The PATH step: (what it says, whether it changed anything); None when this run leaves the PATH alone.
fn path_step(home: &Path, exe: &Path) -> Option<(String, bool)> {
    if no_path() { return None; }
    Some(match pgm_platform_internal::path::ensure_on_path_in(home, exe, "vr-host") {
        Ok(m) => { let changed = !m.starts_with("already"); (format!("vr-host: {m}"), changed) }
        Err(e) => (format!("could not put vr-host on your PATH: {e}"), true),
    })
}
/// The steps for the folder `f` whose program is `exe`, with the home folder given (tests use a scratch one).
pub fn setup_in(f: &Folder, exe: &Path, home: &Path) -> Vec<String> {
    let mut said = vec![];
    if let Some((m, changed)) = path_step(home, exe) { if changed { said.push(m); } }
    // VERSION.txt (a download's note) follows the program it sits next to
    let vt = f.f("VERSION.txt");
    if let Ok(old) = std::fs::read_to_string(&vt) {
        if old.trim() != crate::runner::VERSION && std::fs::write(&vt, format!("{}\n", crate::runner::VERSION)).is_ok() { said.push(format!("VERSION.txt now says {} (it said {})", crate::runner::VERSION, old.trim())); }
    }
    // the previous version's file, once no update is on trial any more (the trial needs it to go back)
    if !f.trial().exists() {
        for n in [exe_name(), hostw_name()] {
            let old = f.f(&format!("{n}.old"));
            if old.exists() && std::fs::remove_file(&old).is_ok() { said.push(format!("removed the previous version's file {n}.old")); }
        }
    }
    // a start-at-boot entry that runs another copy: said, never rewritten from inside the running service
    if !no_boot() {
        let svc = control::service();
        if let Some(p) = pgm_platform_internal::service::program(control::SVC, &svc) {
            if !same_path(&p, exe) { said.push(format!("the start-at-boot entry runs {} but the installed program is {}: `vr-host boot {}` from the installed copy points it here", p.display(), exe.display(), if svc.scope == "user" { "user" } else { "service" })); }
        }
    }
    said
}

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
pub fn install(args: &[&str]) -> i32 { install_to(args, &mut |l| println!("{l}")) }
/// `install`, every line it would print handed to `say` (the control app shows them in its own way).
pub fn install_to(args: &[&str], say: &mut dyn FnMut(String)) -> i32 {
    let me = std::env::current_exe().map(|e| std::fs::canonicalize(&e).unwrap_or(e)).unwrap_or_default();
    let here = me.parent().map(Path::to_path_buf).unwrap_or_default();
    let found = earlier();
    let target = pick_target(&found).unwrap_or_else(standard);
    if in_git(&target) { say(format!("The AI host at {} runs from a git checkout: it updates itself from there; nothing done.", target.display())); return 0; }
    let same = same_path(&target, &here);
    let f = folder_of(&target);
    let target_exe = target.join(exe_name());
    let mut was_running = false;
    if same { say(format!("This is the installed AI host ({}).", target.display())); }
    else {
        was_running = f.host_pid().is_some() || control::service().running;
        if let Some(pid) = f.host_pid() {
            say(format!("An AI host runs there (pid {pid}): asking it to stop (it finishes what is in flight first) …"));
            let _ = std::fs::write(f.stop_now(), "quiet");
            for _ in 0..600 { if f.host_pid().is_none() { break; } std::thread::sleep(std::time::Duration::from_millis(200)); }
        }
        if let Err(e) = std::fs::create_dir_all(&target) { say(format!("Could not make {}: {e}", target.display())); return 1; }
        // the program files; its own settings and logs stay
        for name in [exe_name(), hostw_name(), "README.txt", "VERSION.txt"] {
            let src = here.join(name); if !src.exists() { continue; }
            let dst = target.join(name);
            if dst.exists() && name.starts_with("vr-host") { let old = target.join(format!("{name}.old")); let _ = std::fs::remove_file(&old); let _ = std::fs::rename(&dst, &old); }
            if let Err(e) = std::fs::copy(&src, &dst) { say(format!("Could not copy {name}: {e}")); return 1; }
        }
        // the settings: this download's own (a host's package) only where the install has none yet
        let cfg_src = here.join("ai-node.json");
        if cfg_src.exists() && !target.join("ai-node.json").exists() { let _ = std::fs::copy(&cfg_src, target.join("ai-node.json")); vr_core::settings::private(&target.join("ai-node.json")); }
        let fresh = !f.config.exists();
        let gone = old_shortcuts();
        if gone > 0 { say(format!("Removed {gone} shortcut(s) of the earlier JavaScript host's separate start / stop programs.")); }
        log(format!("installed VibeRoller AI Host (native) {} into {}", crate::runner::VERSION, target.display()));
        say(format!("Installed into {}{}.", target.display(), if fresh { " (no ai-node.json yet: run `vr-host` to set it up, or VibeRoller → Settings → My AI hosts makes one)" } else { "" }));
    }
    // vr-host on the PATH, and whatever else the installed program needs (see ensure_setup)
    match path_step(&home(), &target_exe) { Some((m, _)) => say(m), None => {} }
    let mut svc_lines = setup_in(&f, &target_exe, &home());
    svc_lines.retain(|l| !l.starts_with("vr-host: ") && !l.starts_with("could not put vr-host"));
    for l in svc_lines { say(l); }
    // start at boot: asked for (--boot service | sign-in | user | off), else the entry that exists follows this version, else
    // the default when nothing starts the host yet and there is a config to start it with
    let exe_s = target_exe.display().to_string();
    let mut defaulted = false;
    if no_boot() { say("(start at boot left alone: VR_HOST_NO_BOOT is set)".into()); }
    else if let Some(m) = args.iter().position(|a| *a == "--boot").and_then(|i| args.get(i + 1)) { for l in control::set_boot_for(&f, &exe_s, m) { say(l); } }
    else {
        let svc = control::service();
        if svc.exists {
            // the entry now runs this version (a service pointing at another copy is repointed here)
            if pgm_platform_internal::service::program(control::SVC, &svc).map(|p| same_path(&p, &target_exe)).unwrap_or(false) { say("The start-at-boot entry already runs this program.".into()); }
            else { for l in control::set_boot_for(&f, &exe_s, if svc.scope == "user" { "user" } else { "service" }) { say(l); } }
        } else if control::boot_mode(&svc) == "off" && f.config.exists() {
            say(format!("Nothing starts the host at boot yet: setting the default ({}).", control::boot_label(control::default_boot())));
            for l in control::set_boot_for(&f, &exe_s, control::default_boot()) { say(l); }
            defaulted = true;
        }
    }
    if !no_boot() && f.config.exists() && (was_running || defaulted) && f.host_pid().is_none() && !control::service().running { say(control::start_for(&f, &exe_s)); }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("vr-host-install-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test]
    fn setup_follows_the_program_and_is_idempotent() {
        std::env::set_var("VR_HOST_NO_BOOT", "1");
        let d = scratch("setup"); let home = scratch("setup-home");
        let exe = d.join(exe_name()); std::fs::write(&exe, "x").unwrap();
        std::fs::write(d.join("VERSION.txt"), "0.0.1-old\n").unwrap();
        std::fs::write(d.join(format!("{}.old", exe_name())), "old").unwrap();
        let f = folder_of(&d);
        let first = setup_in(&f, &exe, &home);
        assert!(first.iter().any(|l| l.contains("VERSION.txt now says")), "{first:?}");
        assert!(first.iter().any(|l| l.contains(".old")), "{first:?}");
        assert_eq!(std::fs::read_to_string(d.join("VERSION.txt")).unwrap().trim(), crate::runner::VERSION);
        assert!(!d.join(format!("{}.old", exe_name())).exists());
        if !cfg!(windows) { assert!(first.iter().any(|l| l.contains("linked")), "{first:?}"); assert!(home.join(".local/bin/vr-host").exists()); }
        let second = setup_in(&f, &exe, &home);
        assert!(second.is_empty(), "{second:?}");
        // an update on trial keeps the previous version's file
        std::fs::write(d.join(format!("{}.old", exe_name())), "old").unwrap(); std::fs::write(f.trial(), "{}").unwrap();
        assert!(setup_in(&f, &exe, &home).is_empty() && d.join(format!("{}.old", exe_name())).exists());
        let _ = std::fs::remove_dir_all(&d); let _ = std::fs::remove_dir_all(&home);
    }
    #[test]
    fn a_folder_that_is_not_installed_is_left_alone() {
        let d = scratch("not-installed");
        std::fs::write(d.join(exe_name()), "x").unwrap(); std::fs::write(d.join("VERSION.txt"), "0.0.1-old\n").unwrap();
        // the test program is not in an installed folder, so ensure_setup does nothing at all
        assert!(ensure_setup(&folder_of(&d)).is_empty());
        assert_eq!(std::fs::read_to_string(d.join("VERSION.txt")).unwrap().trim(), "0.0.1-old");
        let _ = std::fs::remove_dir_all(&d);
    }
}
