//! `vr-host machine ...`: this computer as a LINKED MACHINE of its owner's hub (plans/linked-machines.md, T-311). All of it is the
//! library's (`pgm_net_internal::machine`: the link by device code, the poll loop, the tools, the scope, the audit log, the
//! kill switch); this file is only the command line and the optional RE container adapter.
//!
//!   vr-host machine link --hub <url> --folder <dir> [--name <n>]   show a code; approve it on the web, signed in as yourself
//!   vr-host machine run [--hub <url>]                              serve the hub's tool calls until stopped or unlinked
//!   vr-host machine stop                                           the kill switch: no jobs, no link, survives a restart
//!   vr-host machine start                                          start access again (only here, never from the web)
//!   vr-host machine status                                         who it is linked to, the scope in words, stopped or not
//! The state folder (token, device key, scope.json, audit log, KILL): `--state <dir>`, else VR_MACHINE_STATE, else
//! `~/.local/state/vibe-roller/machine` (Linux) / `%LOCALAPPDATA%\VibeRoller\machine` (Windows). The scope is `scope.json` in it.
//! RE containers: when `re.json` is in the state folder (`{ "image": ..., "runtime": "podman" }`, all optional) and Podman or Docker is
//! there, `re_*` tools use the machine's own container manager (pgm-sandbox-internal); without it they say there is no engine.
use pgm_net_internal::machine::exec::Containers;
use pgm_net_internal::machine::kill::{Kill, LocalApproval};
use pgm_net_internal::machine::runner::{self, Ended, LinkRecord, Options, Runner};
use pgm_sandbox_internal::{Actor, Cli, Cmd, Config as SbConfig, Manager, StartOpts, State};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn arg_value(args: &[String], flag: &str) -> Option<String> { args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned()) }

pub fn default_state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("VR_MACHINE_STATE") { return PathBuf::from(d); }
    #[cfg(windows)]
    { std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("VibeRoller").join("machine")).unwrap_or_else(|| PathBuf::from("machine")) }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))).unwrap_or_else(|| PathBuf::from("."));
        base.join("vibe-roller").join("machine")
    }
}

fn now() -> i64 { (pgm_core_internal::now_ms() / 1000) as i64 }

pub fn run(args: &[String]) -> i32 {
    let i = args.iter().position(|a| a == "machine").unwrap_or(0);
    let sub = args.get(i + 1).map(String::as_str).unwrap_or("");
    let state = arg_value(args, "--state").map(PathBuf::from).unwrap_or_else(default_state_dir);
    let hub = arg_value(args, "--hub").unwrap_or_default();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio");
    let mut o = Options::new(&hub, &state, "vr-host");
    o.app_version = env!("CARGO_PKG_VERSION").to_string();
    match sub {
        "link" => {
            let Some(folder) = arg_value(args, "--folder").map(|f| std::path::absolute(&f).unwrap_or_else(|_| PathBuf::from(f))) else { eprintln!("vr-host machine link --hub <url> --folder <dir>: the folder is the one the AI may use"); return 2 };
            if hub.is_empty() { eprintln!("vr-host machine link --hub <url> --folder <dir>"); return 2; }
            if !folder.is_dir() { eprintln!("{} is not a folder", folder.display()); return 2; }
            let name = arg_value(args, "--name").unwrap_or_else(|| std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")).unwrap_or_else(|_| "this computer".into()));
            match rt.block_on(runner::link(&o, &folder.display().to_string(), &name, &|code| { println!("Approve this machine on the web (Settings, My machines), signed in as yourself.\nCode: {code}"); use std::io::Write; let _ = std::io::stdout().flush(); })) {
                Ok(r) => { println!("Linked to {} as machine {}. Scope: one folder, {}, read-write.", r.owner, r.machine, folder.display()); 0 }
                Err(e) => { eprintln!("not linked: {e}"); 1 }
            }
        }
        "run" => {
            let r = match Runner::open(o) { Ok(r) => r, Err(e) => { eprintln!("{e}"); return 1; } };
            if let Some(re) = ReContainers::from_state(&state) { r.machine().set_containers(Some(Arc::new(re))); }
            println!("serving {}'s tool calls for machine {} (scope: {})", r.record().owner, r.record().machine, r.machine().summary());
            let r2 = r.clone();
            match rt.block_on(async move { tokio::select! { e = r2.run() => e, _ = tokio::signal::ctrl_c() => Ended::Cancelled } }) {
                Ended::Revoked => { eprintln!("this machine was unlinked on the web: its token and key are wiped; link it again to use it"); 3 }
                Ended::Cancelled => { r.machine().stop_jobs(); 0 }
            }
        }
        "stop" => match Kill::open(&state).stop("vr-host machine stop", "stopped from the terminal", now()) { Ok(()) => { println!("stopped: no jobs, no link, until `vr-host machine start` on this computer"); 0 } Err(e) => { eprintln!("{e}"); 1 } },
        "start" => match Kill::open(&state).clear(&LocalApproval::from_machine("vr-host machine start")) { Ok(was) => { println!("{}", if was { "access is on again" } else { "it was not stopped" }); 0 } Err(e) => { eprintln!("{e}"); 1 } },
        "status" => {
            let k = Kill::open(&state);
            match LinkRecord::load(&state) {
                Some(l) => println!("linked to {} as machine {} on {}", l.owner, l.machine, l.hub),
                None => println!("not linked"),
            }
            println!("{}", if k.is_stopped() { format!("STOPPED ({})", k.info().unwrap_or_default()) } else { "access on".to_string() });
            println!("scope: {}", pgm_net_internal::machine::Scope::load(&state.join("scope.json")).summary(now()));
            0
        }
        _ => { eprintln!("vr-host machine link | run | stop | start | status   (--state <dir>, --hub <url>)"); 2 }
    }
}

/// The machine's own RE containers: pgm-sandbox-internal's manager over Podman / Docker, for the account that owns the machine.
pub struct ReContainers { mgr: Arc<Manager> }

impl ReContainers {
    /// Present when `<state>/re.json` exists.
    pub fn from_state(state: &Path) -> Option<ReContainers> {
        let c: Value = serde_json::from_str(&std::fs::read_to_string(state.join("re.json")).ok()?).ok()?;
        let root = c["exchangeDir"].as_str().filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| state.join("re"));
        let mgr = Arc::new(Manager::new(Box::new(Cli::named(c["runtime"].as_str().unwrap_or("auto"))), SbConfig::from_json(root, &c)));
        let m2 = mgr.clone();
        std::thread::spawn(move || { let _ = m2.reconcile(); loop { std::thread::sleep(std::time::Duration::from_secs(30)); m2.tick(); } });
        Some(ReContainers { mgr })
    }
}

impl Containers for ReContainers {
    fn available(&self) -> Result<(), String> { self.mgr.ready() }
    fn call(&self, tool: &str, a: &Value, who: &str) -> Result<Value, String> {
        let who = Actor::new(who, false);
        let m = &self.mgr;
        let e = |x: pgm_sandbox_internal::Error| x.msg;
        match tool {
            "re_list" => { let (active, queued, max) = m.counts(); Ok(json!({ "sandboxes": m.list(), "running": active, "queued": queued, "max": max, "image": m.config().image, "runtime": m.runtime_name() })) }
            "re_logs" => { let id = a["id"].as_str().unwrap_or(""); let s = m.status(id).map_err(e)?; Ok(json!({ "sandbox": s, "files": m.files(id) })) }
            "re_stop" => { let id = a["id"].as_str().unwrap_or(""); Ok(json!({ "sandbox": m.stop(&who, id).map_err(e)?, "deleted": true })) }
            "re_run" => {
                let Some(cmd) = Cmd::from_json(&a["cmd"]) else { return Err("cmd is a command line (string) or a list of arguments.".into()) };
                let id = match a["id"].as_str().filter(|s| !s.is_empty()) {
                    Some(id) => id.to_string(),
                    None => {
                        // a new copy, within the manager's limit of 3 running plus its queue (Sponge, 11)
                        let s = m.start(&who, a["purpose"].as_str().unwrap_or(""), &StartOpts { network: false, image: None }).map_err(e)?;
                        if s.state == State::Gone { return Err(s.error.unwrap_or_else(|| "The copy did not start.".into())); }
                        if s.state == State::Queued { return Ok(json!({ "sandbox": s, "hint": "Queued: call re_logs until its state is running, then re_run with this id." })); }
                        s.id
                    }
                };
                let r = m.exec(&who, &id, &cmd, a["timeoutSec"].as_u64()).map_err(e)?;
                Ok(json!({ "id": id, "result": r }))
            }
            t => Err(format!("Unknown tool {t}")),
        }
    }
}
