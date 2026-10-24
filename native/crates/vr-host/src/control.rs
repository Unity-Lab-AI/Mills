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
/// The installed service / unit / sign-in entry (VR_HOST_NO_BOOT=1: none, and nothing is read from the machine).
pub fn service() -> Service { if crate::install::no_boot() { Service::default() } else { pgm_platform_internal::service::status(SVC) } }
fn names() -> pgm_platform_internal::service::ServiceSpec { pgm_platform_internal::service::ServiceSpec { name: SVC.into(), display: "VibeRoller AI Host".into(), ..Default::default() } }
pub fn boot_mode(s: &Service) -> &'static str { pgm_platform_internal::service::boot_mode(&names(), s) }
pub fn boot_label(m: &str) -> String { pgm_platform_internal::service::boot_label(&names(), m) }
pub fn boot_modes() -> &'static [&'static str] { pgm_platform_internal::service::boot_modes() }

/// The command line the service / the background start / the user unit run.
fn run_line(f: &Folder, exe: &str) -> (String, Vec<String>) { (exe.to_string(), vec!["run".into(), "--config".into(), f.config.display().to_string()]) }
/// The VibeRoller AI Host as a service, running the program at `exe`.
fn spec(f: &Folder, exe: &str) -> pgm_platform_internal::service::ServiceSpec {
    let (exe, args) = run_line(f, exe);
    pgm_platform_internal::service::ServiceSpec {
        name: SVC.into(), display: "VibeRoller AI Host".into(),
        description: "Lends this PC's AI (its Claude / ChatGPT login, local models, Unity's engines) to the VibeRoller hub, never the computer itself. Starts at boot.".into(),
        // the service runs as LocalSystem; the logins it lends are this person's (their profile folder is passed on)
        service_bin: format!("\"{exe}\" service-run --config \"{}\" --home \"{}\"", f.config.display(), home().display()),
        signin_exe: hostw(&exe), elevate_args: format!("boot {{mode}} --config \"{}\"", f.config.display()),
        exe, args, dir: f.dir.clone(), log: f.log(), log_name: "ai-node.log".into(),
    }
}
pub fn set_boot(f: &Folder, mode: &str) -> Vec<String> { set_boot_for(f, &exe(), mode) }
/// Start at boot for the program at `exe` (an install sets it for the installed program, not for the download it runs from).
pub fn set_boot_for(f: &Folder, exe: &str, mode: &str) -> Vec<String> {
    if crate::install::no_boot() { return vec!["(start at boot left alone: VR_HOST_NO_BOOT is set)".into()]; }
    pgm_platform_internal::service::set_boot(&spec(f, exe), mode)
}
/// What starts the host when nothing does yet: Windows at sign-in (no administrator), Linux a user service (no sudo).
pub fn default_boot() -> &'static str { if WIN { "sign-in" } else { "user" } }
/// The window-less starter next to this program on Windows (vr-hostw.exe), else this program.
pub fn hostw(exe: &str) -> String {
    let p = PathBuf::from(exe).with_file_name(if WIN { "vr-hostw.exe" } else { "vr-hostw" });
    if p.exists() { p.display().to_string() } else { exe.to_string() }
}

pub fn start(f: &Folder) -> String { start_for(f, &exe()) }
/// Starts the host, with the program at `exe` when it has to be started by hand.
pub fn start_for(f: &Folder, exe: &str) -> String {
    let _ = std::fs::remove_file(f.off());
    let svc = service();
    if svc.exists {
        let (ok, out) = pgm_platform_internal::service::start(SVC, &svc);
        return if ok || out.contains("1056") || out.to_lowercase().contains("already") { "Started (the service runs it).".into() } else { format!("Could not start the service: {}", out.trim().lines().last().unwrap_or("")) };
    }
    if let Some(pid) = f.host_pid() { return format!("It is running already (pid {pid})."); }
    if !f.config.exists() { return "No ai-node.json here yet: set the hub, id and token first ([c] settings), or download your own host's package.".into(); }
    match spawn_background_for(f, exe) { Ok(()) => "Started (in the background; its log is ai-node.log).".into(), Err(e) => format!("Could not start it: {e}") }
}
/// `vr-host run` in the background, its output in ai-node.log, no window.
pub fn spawn_background_for(f: &Folder, exe: &str) -> std::io::Result<()> { let (exe, args) = run_line(f, exe); pgm_platform_internal::proc::spawn_detached(&exe, &args, &f.dir, &f.log()) }
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
pub struct Snapshot { pub cfg: Option<HostSettings>, pub st: Value, pub pid: Option<u32>, pub svc: Service, pub live: bool, pub connected: bool, pub paused: bool, pub boot: &'static str, pub version: String,
    /// each login-relay provider (Claude / Codex) with why it cannot answer, if it cannot
    pub logins: Vec<(String, Option<String>)> }
/// What `login_problem` says, short enough for a dashboard row (its first sentence).
pub fn short(s: &str) -> String { let first = s.split(". ").next().unwrap_or(s); let t: String = first.chars().take(80).collect(); if first.chars().count() > 80 { format!("{t}…") } else { t } }
/// Each enabled login-relay provider with why its login does not work (None = it works).
pub fn logins(cfg: Option<&HostSettings>) -> Vec<(String, Option<String>)> {
    cfg.map(|c| c.all_providers().into_iter().filter(|p| p.enabled && p.is_relay()).map(|p| { let pr = pgm_ai_internal::host::relay::login_problem(&p); (p.id, pr) }).collect()).unwrap_or_default()
}
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
    let logins = logins(cfg.as_ref());
    Snapshot { paused: f.off().exists(), cfg, st, pid, svc, live, connected, boot, version, logins }
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
    let hub_v = st["hubVersion"].as_str().filter(|v| !v.is_empty());
    let newer = s.live && hub_v.map(|v| v != s.version).unwrap_or(false);
    r.push(("Updates".into(), format!("{} · running {}{} · last: {}", if cfg.map(|c| c.auto_update()).unwrap_or(true) { "automatic" } else { "automatic updates OFF" }, s.version,
        hub_v.filter(|_| s.live).map(|v| if newer { format!(" · hub has {v} (a different version: it updates itself at a quiet moment)") } else { format!(" · hub {v} (same)") }).unwrap_or_default(),
        st["lastUpdate"].as_str().filter(|u| !u.is_empty()).map(|u| format!("{u} ({})", ago(n("lastUpdateAt")))).unwrap_or_else(|| "none yet".into())), if cfg.map(|c| c.auto_update()).unwrap_or(true) { "" } else { "warn" }));
    // each provider with its login mark, and the Ollama this machine has
    {
        let mut parts: Vec<String> = vec![];
        for p in cfg.map(|c| c.all_providers()).unwrap_or_default() {
            let mark = if !p.enabled { "· (off) ".to_string() } else if p.is_relay() { match s.logins.iter().find(|(i, _)| *i == p.id).and_then(|(_, pr)| pr.as_ref()) { Some(pr) => format!("✗ ({}) ", short(pr)), None => "✓ ".into() } } else { format!("· ({} model(s)) ", p.models.len()) };
            parts.push(format!("{} {}", p.id, mark.trim_end()));
        }
        if let Some(u) = st["detected"]["ollama"].as_str() { parts.push(format!("Ollama at {u}: {} model(s) found on this machine", st["detected"]["models"].as_array().map(|a| a.len()).unwrap_or(0))); }
        let bad = s.logins.iter().any(|(_, p)| p.is_some());
        r.push(("Providers".into(), if parts.is_empty() { "none".into() } else { parts.join("  ·  ") }, if bad { "warn" } else { "" }));
    }
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

/// One line of `vr-host doctor`: ok Some(true) = fine, Some(false) = needs fixing, None = information. `fix` names the
/// repair `control::fix` can do ("lockdown", "path", "boot", "install"); a row without one says in its text what to do.
#[derive(Clone, Debug)]
pub struct Check { pub id: &'static str, pub ok: Option<bool>, pub text: String, pub fix: Option<&'static str> }
fn ck(id: &'static str, ok: Option<bool>, text: impl Into<String>, fix: Option<&'static str>) -> Check { Check { id, ok, text: text.into(), fix } }
impl Check {
    pub fn tag(&self) -> &'static str { match self.ok { Some(true) => "OK  ", Some(false) => "FIX ", None => "INFO" } }
    pub fn json(&self) -> Value { serde_json::json!({ "id": self.id, "ok": self.ok, "text": self.text, "fix": self.fix }) }
}
/// The Ollama on this machine as (address, models), asked now (1.5 s at most).
pub fn probe_ollama() -> Option<(String, Vec<String>)> { pgm_ai_internal::host::runner::probe_ollama_blocking() }

/// Everything that has to be right for the host to work, one check each, none of them talking to the hub (the hub is
/// told by the host's own status file). Asks the local Ollama too.
pub fn doctor(f: &Folder) -> Vec<Check> { doctor_with(f, true) }
/// `doctor`, with the Ollama question optional (tests).
pub fn doctor_with(f: &Folder, ask_ollama: bool) -> Vec<Check> {
    let mut c = vec![];
    let snap = snapshot(f);
    let cp = crate::install::copies();
    // the settings
    match HostSettings::load(&f.config) {
        Err(_) if !f.config.exists() => c.push(ck("config", Some(false), format!("No ai-node.json in {}: the host has no hub, id and token yet. Run `vr-host` to set them up (or paste your host's ai-node.json there).", f.dir.display()), None)),
        Err(e) => c.push(ck("config", Some(false), format!("ai-node.json cannot be used: {e}"), None)),
        Ok(cfg) => c.push(match cfg.check() {
            Ok(()) => ck("config", Some(true), format!("Settings are complete: host \"{}\" at {}", cfg.id(), cfg.hub()), None),
            Err(e) => ck("config", Some(false), format!("{e} Fix it in the settings screen ([c]) or with `vr-host set`."), None),
        }),
    }
    match config_private(f) {
        Some(true) => c.push(ck("private", Some(true), "ai-node.json is readable by you only (it holds the token)", None)),
        Some(false) => c.push(ck("private", Some(false), "ai-node.json can be read by other users on this machine, and it holds the token", Some("lockdown"))),
        None => {}
    }
    // which copy is this
    let here = cp.this_exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    c.push(if cp.this_installed { ck("copy", Some(true), format!("This is the installed AI host (version {}, in {here})", crate::runner::VERSION), None) }
        else if let Some(i) = &cp.installed { ck("copy", Some(false), format!("This is a download copy (version {}, in {here}). The installed host in {} {} version {}.", crate::runner::VERSION, i.dir.display(), if cp.installed_running { "runs" } else { "is stopped and has" }, cp.installed_version), Some("install")) }
        else { ck("copy", Some(false), format!("This program runs from {here} and is not installed. Install puts it in {} (so it can update itself and start at boot).", crate::install::standard().display()), Some("install")) });
    // start at boot: there is an entry, and it runs the installed program
    let want_exe = cp.installed_exe();
    let program = pgm_platform_internal::service::program(SVC, &snap.svc);
    c.push(if snap.boot == "off" { ck("boot", Some(false), "Nothing starts the host at boot or sign-in: it runs only when started by hand", Some("boot")) }
        else if let Some(p) = program.filter(|p| !same_file(p, &want_exe)) { ck("boot", Some(false), format!("The start-at-boot entry runs {} but the installed program is {}", p.display(), want_exe.display()), Some("boot")) }
        else { ck("boot", Some(true), format!("Starts at boot: {}", boot_label(snap.boot)), None) });
    // vr-host on the PATH
    let (path_ok, path_text) = pgm_platform_internal::path::path_status(&want_exe, "vr-host");
    c.push(ck("path", Some(path_ok), path_text, if path_ok { None } else { Some("path") }));
    // the hub, as the running host last said
    c.push(if snap.pid.is_none() { ck("hub", None, "The host is not running (press [s] to start it); nothing to say about the hub yet", None) }
        else if snap.connected { ck("hub", Some(true), format!("Connected to {} (heartbeat {})", snap.cfg.as_ref().map(|x| x.hub()).unwrap_or_default(), ago(snap.st["lastOkAt"].as_u64().unwrap_or(0))), None) }
        else { ck("hub", Some(false), format!("Running but not connected{}", snap.st["lastError"].as_str().filter(|e| !e.is_empty()).map(|e| format!(": {e}")).unwrap_or_default()), None) });
    // each login the host lends
    for (id, problem) in &snap.logins {
        c.push(match problem { Some(p) => ck("login", Some(false), format!("Provider \"{id}\": {p}"), None), None => ck("login", Some(true), format!("Provider \"{id}\": its login is there"), None) });
    }
    // a local Ollama
    if ask_ollama {
        let offered = snap.st["detected"]["ollama"].as_str().is_some() || snap.cfg.as_ref().map(|x| x.engines().iter().any(|(k, _)| k == "ollama") || x.all_providers().iter().any(|p| p.kind == "openai" && (p.id == "ollama" || p.base_url.contains(":11434")))).unwrap_or(false);
        c.push(match probe_ollama() {
            Some((url, models)) => ck("ollama", Some(true), format!("Ollama answers at {url} with {} model(s){}", models.len(), if offered { "; this host offers it" } else if snap.live { "; the running host offers it within a minute" } else { "; the host offers it once it runs" }), None),
            None => ck("ollama", None, format!("No Ollama answers at {} (optional: it lends local models; start `ollama serve`)", pgm_ai_internal::host::runner::ollama_address()), None),
        });
    }
    // updates
    let auto = snap.cfg.as_ref().map(|x| x.auto_update()).unwrap_or(true);
    let hub_v = snap.st["hubVersion"].as_str().filter(|v| !v.is_empty()).unwrap_or("");
    c.push(if !auto { ck("updates", Some(false), "Automatic updates are OFF: this host stays on its version until you update it (turn them on in [c] settings: Automatic updates)", None) }
        else { ck("updates", Some(true), format!("Automatic updates are on (this program {}{})", crate::runner::VERSION, if snap.live && !hub_v.is_empty() { if hub_v == snap.version { format!("; the hub is on {hub_v}, the same") } else { format!("; the hub is on {hub_v}: it updates itself at a quiet moment") } } else { String::new() }), None) });
    c
}
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool { a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y) }

/// Does the repair a FIX row names (see Check::fix); what happened, in words. It may ask for sudo / administrator rights:
/// the caller gives the terminal back first.
pub fn fix(f: &Folder, id: &str) -> String {
    match id {
        "lockdown" => lockdown(f),
        "path" => {
            let exe = crate::install::copies().installed_exe();
            match pgm_platform_internal::path::ensure_on_path(&exe, "vr-host") { Ok(m) => format!("vr-host: {m}"), Err(e) => format!("Could not put vr-host on the PATH: {e}") }
        }
        "boot" => {
            let cp = crate::install::copies();
            if !cp.this_installed { return "Start at boot is set from the installed copy: install this one first (the \"install\" fix), then run `vr-host` from there.".into(); }
            let m = match boot_mode(&service()) { "off" => default_boot(), m => m };
            let mut lines = set_boot(f, m);
            if f.host_pid().is_none() && !service().running && f.config.exists() { lines.push(start(f)); }
            lines.join(" ")
        }
        "install" => { let mut lines = vec![]; crate::install::install_to(&["install"], &mut |l| lines.push(l)); lines.join(" ") }
        _ => format!("Nothing to fix for \"{id}\"."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch(tag: &str) -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("vr-host-doctor-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn row<'a>(c: &'a [Check], id: &str) -> Option<&'a Check> { c.iter().find(|x| x.id == id) }
    #[cfg(unix)]
    #[test]
    fn the_doctor_reads_a_folder() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("folder");
        let f = Folder { dir: d.clone(), config: d.join("ai-node.json") };
        let none = doctor_with(&f, false);
        assert_eq!(row(&none, "config").map(|c| c.ok), Some(Some(false)));
        assert!(row(&none, "private").is_none() && row(&none, "path").is_some() && row(&none, "updates").is_some());
        let cfg = serde_json::json!({ "hub": "https://portal.example.org", "id": "t", "token": "t".repeat(30), "providers": [{ "id": "codex", "kind": "codex-relay", "codexHome": d.join("nope").display().to_string() }] });
        std::fs::write(&f.config, cfg.to_string()).unwrap();
        std::fs::set_permissions(&f.config, std::fs::Permissions::from_mode(0o644)).unwrap();
        let readable = doctor_with(&f, false);
        assert_eq!(row(&readable, "config").map(|c| c.ok), Some(Some(true)));
        let p = row(&readable, "private").unwrap();
        assert_eq!((p.ok, p.fix), (Some(false), Some("lockdown")));
        assert_eq!(row(&readable, "login").map(|c| c.ok), Some(Some(false)));
        assert_eq!(row(&readable, "hub").map(|c| c.ok), Some(None));
        assert!(fix(&f, "lockdown").contains("readable by you only"));
        let private = doctor_with(&f, false);
        assert_eq!(row(&private, "private").map(|c| c.ok), Some(Some(true)));
        let j = row(&private, "config").unwrap().json();
        assert!(j["id"] == "config" && j["ok"] == true && j["fix"].is_null());
        assert_eq!(row(&private, "config").unwrap().tag(), "OK  ");
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn short_reasons_and_unknown_fixes() {
        assert_eq!(short("no Codex login found at /x/auth.json. Sign in with codex login"), "no Codex login found at /x/auth.json");
        assert!(short(&"a".repeat(200)).chars().count() <= 81);
        let d = scratch("fix"); let f = Folder { dir: d.clone(), config: d.join("ai-node.json") };
        assert!(fix(&f, "nope").contains("Nothing to fix"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
