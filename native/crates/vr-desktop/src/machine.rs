//! This computer as a LINKED MACHINE of the app's hub (plans/linked-machines.md, T-313). Everything that matters is the library's
//! (`pgm_net_internal::machine`: the device-code link, the runner, the tools, the scope, the audit log, the kill switch, the
//! service template and the owner-only state folder check); this file is only the app's two modes around it:
//! - while open: "Link this computer" runs the device-code link, and the runner serves the hub's tool calls on a thread of this
//!   app until the app closes or the person pauses it;
//! - persistent: "Keep it running when the app is closed" calls the bundled `vr-host machine install` (a systemd user unit on
//!   Linux, a per-user Scheduled Task on Windows; no root, no administrator). The two are never on at once.
//! Only the Tools window (this app's own page) may call these commands. The hub's page may not (capabilities/, main.rs): linking
//! a computer and widening what the AI may reach is the person's act, on the machine, never the web page's.
//! The state folder is `<app data>/machine` (so the VibeRoller and Hyper Prod Man builds, each with its own hub and account, keep
//! separate links). Linux and Windows only.
use pgm_net_internal::machine::kill::Kill;
use pgm_net_internal::machine::runner::{self, Ended, LinkRecord, Options, Runner};
use pgm_net_internal::machine::service::{check_state_dir, Unit};
use pgm_net_internal::machine::Scope;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What this build is called: "VibeRoller", or a PGM product's name (VR_APP_NAME, native/build-products.sh).
pub const APP_NAME: &str = match option_env!("VR_APP_NAME") { Some(n) => n, None => "VibeRoller" };

#[derive(Default, Clone)]
struct Linking { active: bool, code: String, error: String }

#[derive(Default)]
pub struct MachineApp {
    run: Mutex<Option<Arc<Runner>>>,
    linking: Mutex<Linking>,
    last: Mutex<String>,
}

fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

pub fn state_dir(app_data: &Path) -> PathBuf { app_data.join("machine") }

fn slug() -> String {
    let s: String = APP_NAME.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.trim_matches('-').to_string()
}
/// The service of this build: `vr-host machine run`, named after the app.
pub fn unit(state: &Path) -> Unit {
    Unit::new(&format!("{}-machine", slug()), &format!("{APP_NAME} linked machine"), &crate::aihost::host_exe(), &["machine", "run"], state)
}
fn options(hub: &str, state: &Path) -> Options {
    let mut o = Options::new(hub, state, &format!("{}-desktop", slug()));
    o.app_version = crate::update::VERSION.to_string();
    o.mode = "while open".into();
    o
}
fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }

impl MachineApp {
    pub fn status(&self, state: &Path, hub: &str) -> Value {
        let l = lk(&self.linking).clone();
        let rec = LinkRecord::load(state);
        let kill = Kill::open(state);
        let service = unit(state).installed();
        json!({
            "app": APP_NAME, "hub": hub, "stateDir": state.display().to_string(),
            "linked": rec.is_some(), "owner": rec.as_ref().map(|r| r.owner.clone()), "machine": rec.as_ref().map(|r| r.machine.clone()), "linkedHub": rec.as_ref().map(|r| r.hub.clone()),
            "whileOpen": lk(&self.run).is_some(), "service": service,
            "mode": if service { "persistent" } else if lk(&self.run).is_some() { "while open" } else { "off" },
            "scope": Scope::load(&state.join("scope.json")).summary(now()),
            "stopped": kill.is_stopped(), "stoppedInfo": kill.info(),
            "linking": { "active": l.active, "code": l.code, "error": l.error },
            "stateError": check_state_dir(state).err(), "last": lk(&self.last).clone(),
            "exeThere": crate::aihost::host_exe().exists(),
        })
    }

    /// Starts the device-code link on its own thread and returns the code to show (or why it could not start). The person approves it on
    /// the web, signed in as themselves; the link then starts the while-open runner by itself.
    pub fn link(self: &Arc<Self>, hub: &str, state: &Path, folder: &str, name: &str, on_linked: Arc<dyn Fn() + Send + Sync>) -> Result<String, String> {
        if hub.is_empty() { return Err("This app has no hub address.".into()); }
        let folder = std::path::absolute(folder).map_err(|e| e.to_string())?;
        if !folder.is_dir() { return Err(format!("{} is not a folder.", folder.display())); }
        if lk(&self.linking).active { return Err("A link is already waiting for approval.".into()); }
        check_state_dir(state)?;
        *lk(&self.linking) = Linking { active: true, ..Default::default() };
        let (me, hub, state, name) = (self.clone(), hub.to_string(), state.to_path_buf(), if name.trim().is_empty() { std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")).unwrap_or_else(|_| "this computer".into()) } else { name.trim().to_string() });
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio");
            let o = options(&hub, &state);
            let me2 = me.clone();
            let r = rt.block_on(runner::link(&o, &folder.display().to_string(), &name, &move |code| { lk(&me2.linking).code = code.to_string(); }));
            let mut l = lk(&me.linking);
            match r {
                Ok(rec) => { l.active = false; l.code.clear(); l.error.clear(); *lk(&me.last) = format!("Linked to {} as machine {}.", rec.owner, rec.machine); drop(l); on_linked(); }
                Err(e) => { l.active = false; l.code.clear(); l.error = e; }
            }
        });
        // the code appears within a second or two (the hub answers the start call)
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let l = lk(&self.linking).clone();
            if !l.code.is_empty() { return Ok(l.code); }
            if !l.error.is_empty() { return Err(l.error); }
        }
        Err("The hub did not answer the link request.".into())
    }

    /// While-open mode: the runner serves the hub's calls on a thread of this app.
    pub fn start(self: &Arc<Self>, hub: &str, state: &Path) -> Result<(), String> {
        if unit(state).installed() { return Err("The persistent service is installed: it already serves this computer. Remove it first to use the while-open mode.".into()); }
        let mut slot = lk(&self.run);
        if slot.is_some() { return Ok(()); }
        let r = Runner::open(options(hub, state))?;
        *slot = Some(r.clone());
        drop(slot);
        let me = self.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
            let ended = rt.block_on(r.clone().run());
            r.machine().stop_jobs();
            *lk(&me.run) = None;
            *lk(&me.last) = match ended { Ended::Revoked => "This computer was unlinked on the web: its token and key are wiped. Link it again to use it.".into(), Ended::Cancelled => "Paused: the hub cannot reach this computer.".into() };
        });
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(r) = lk(&self.run).clone() { r.cancel(); r.machine().stop_jobs(); }
    }

    /// Persistent mode: the bundled vr-host installs / removes the service for this state folder. Never with the while-open runner on.
    pub fn service(&self, state: &Path, install: bool) -> Result<String, String> {
        let exe = crate::aihost::host_exe();
        if !exe.exists() { return Err(format!("The AI host program is not next to this app ({}): the service needs it.", exe.display())); }
        if install {
            if LinkRecord::load(state).is_none() { return Err("Link this computer first.".into()); }
            self.stop();
            for _ in 0..40 { if lk(&self.run).is_none() { break; } std::thread::sleep(std::time::Duration::from_millis(100)); }
        }
        let u = unit(state);
        let mut c = std::process::Command::new(&exe);
        c.args(["machine", if install { "install" } else { "uninstall" }, "--state"]).arg(state).args(["--name", &u.name, "--display", &u.display]).stdin(std::process::Stdio::null());
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
        let o = c.output().map_err(|e| e.to_string())?;
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_string();
        if o.status.success() { Ok(text) } else { Err(text) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_service_is_named_after_the_app_and_runs_the_bundled_host() {
        let u = unit(Path::new("/x/machine"));
        assert!(u.name.ends_with("-machine") && !u.name.contains(' '), "{}", u.name);
        assert!(u.systemd_unit().contains("machine run --state /x/machine") && u.systemd_unit().contains("Restart=on-failure"));
        assert!(u.display.contains(APP_NAME));
    }
    #[test]
    fn the_status_of_an_unlinked_folder_says_so() {
        let d = std::env::temp_dir().join(format!("vr-desk-machine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let m = Arc::new(MachineApp::default());
        let s = m.status(&d, "https://hub.example");
        assert_eq!(s["linked"], json!(false));
        assert_eq!(s["mode"], json!("off"));
        assert!(m.start("https://hub.example", &d).is_err(), "not linked: the runner does not start");
        let _ = std::fs::remove_dir_all(&d);
    }
}
