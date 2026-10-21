//! vr-host: the VibeRoller AI Host, native (Windows + Linux), and its heart & knife control app in one program.
//!
//!   vr-host                         the control app (a terminal dashboard); without a terminal: the status
//!   vr-host run [--config <file>]   the host, supervised: restarted on new code / an update (exit 3) and after a crash
//!   vr-host status | start | stop [--now] | restart [--now] | pause | resume | update | log [n] | lockdown
//!           boot service | sign-in | user | off      settings      set <setting> <value>      version
//!   vr-host install [--boot <mode>] install / update this download into the standard folder (an earlier install is
//!                                   found and kept: its settings and logs stay)
//!   vr-host --selftest              load everything and exit 0 (an update is tried this way first)
//!   vr-host mcp pc|board|team       the team tools for a Work run's Claude Code (MCP over stdio)
//! It works in the same folder, with the same ai-node.json and the same files, as the JavaScript host
//! (bridge/ai-node.js + bridge/vr-host.js), so it can take that one's place, and either control app reads either host.
mod control;
mod files;
mod install;
mod mcp;
mod proc;
mod relay;
mod runner;
mod tui;
mod work;
#[cfg(windows)]
mod winservice;
#[cfg(windows)]
mod winsys;

use files::Folder;

fn arg_value(args: &[String], flag: &str) -> Option<String> { args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned()) }

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = arg_value(&args, "--config");
    if let Some(h) = arg_value(&args, "--home") { std::env::set_var("VR_LOGIN_HOME", h); }
    let folder = Folder::find(config.as_deref());
    let rt = || tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio");
    if args.iter().any(|a| a == "--selftest") { std::process::exit(rt().block_on(runner::run(folder, true, false))); }
    let words: Vec<&str> = { let mut skip = false; args.iter().filter(|a| { if skip { skip = false; return false; } if *a == "--config" || *a == "--home" { skip = true; return false; } !a.starts_with("--") }).map(String::as_str).collect() };
    let now = args.iter().any(|a| a == "--now");
    let code = match words.first().copied() {
        None if args.iter().any(|a| a == "--update") => rt().block_on(runner::run(folder, false, true)),
        None => { if std::io::IsTerminal::is_terminal(&std::io::stdin()) { tui::run(&folder) } else { command(&folder, &["status"], now) } }
        Some("run") => supervise(&folder, None),
        Some("host") => rt().block_on(runner::run(folder, false, args.iter().any(|a| a == "--update"))),
        Some("mcp") => rt().block_on(mcp::serve(words.get(1).copied().unwrap_or(""))),
        #[cfg(windows)]
        Some("service-run") => winservice::dispatch(folder),
        Some(_) => command(&folder, &words, now),
    };
    std::process::exit(code);
}

fn command(f: &Folder, w: &[&str], now: bool) -> i32 {
    let out = |s: String| println!("{s}");
    match w[0] {
        "status" if std::env::args().any(|a| a == "--json") => { // for the desktop app's AI host switch
            let s = control::snapshot(f);
            out(serde_json::json!({ "running": s.pid.is_some(), "pid": s.pid, "connected": s.connected, "paused": s.paused, "boot": s.boot, "configured": s.cfg.is_some(),
                "nickname": s.st["nickname"], "hub": s.cfg.as_ref().map(|c| c.hub()), "version": s.version, "lastError": s.st["lastError"], "active": s.st["active"], "lastOkAt": s.st["lastOkAt"] }).to_string());
            if s.pid.is_some() { 0 } else { 3 }
        }
        "status" => { let s = control::snapshot(f); out(format!("VibeRoller AI Host (native) {} · {}", s.version, f.dir.display())); for (l, t, _) in control::rows(&s, f) { out(format!("{l:<10} {t}")); } if s.pid.is_some() { 0 } else { 3 } }
        "start" => { out(control::start(f)); 0 }
        "stop" => { out(control::stop(f, now)); 0 }
        "restart" => { out(control::restart(f, now)); 0 }
        "pause" => { out(control::pause(f)); 0 }
        "resume" => { out(control::resume(f)); 0 }
        "update" => { out(control::update_now(f)); 0 }
        "log" => { for l in control::log_tail(f, w.get(1).and_then(|n| n.parse().ok()).unwrap_or(40)) { out(l); } 0 }
        "lockdown" => { out(control::lockdown(f)); 0 }
        "boot" => {
            let m = w.get(1).copied().unwrap_or("");
            if !control::boot_modes().contains(&m) { out(format!("boot {}", control::boot_modes().join(" | "))); return 1; }
            for l in control::set_boot(f, m) { out(l); } 0
        }
        "settings" => match vr_core::settings::HostSettings::load(&f.config) { Ok(c) => { for (k, v, l) in c.list() { out(format!("{k:<34} {v:<24} {l}")); } 0 } Err(e) => { out(e.to_string()); 1 } },
        "set" => {
            let (Some(k), true) = (w.get(1), w.len() >= 2) else { out("vr-host set <setting> <value>".into()); return 1 };
            let v = w[2..].join(" ");
            match vr_core::settings::HostSettings::load(&f.config).and_then(|mut c| { c.set(k, &v)?; c.save()?; Ok(c) }) {
                Ok(c) => { let shown = c.list().into_iter().find(|(key, _, _)| key == k).map(|(_, v, l)| format!("{l}: {v}.")).unwrap_or_default(); out(format!("{shown} {}", if f.host_pid().is_some() { "The host applies it at its next quiet moment." } else { "" })); 0 }
                Err(e) => { out(e.to_string()); 1 }
            }
        }
        "version" => { out(format!("{} {} {}", runner::VERSION, runner::COMMIT, runner::TARGET)); 0 }
        "install" => { let all: Vec<String> = std::env::args().collect(); install::install(&all.iter().map(String::as_str).collect::<Vec<_>>()) }
        "tools" => { out(mcp::tool_names().to_string()); 0 }
        _ => { out("vr-host [run | status | start | stop [--now] | restart [--now] | pause | resume | update | log [n] | lockdown | boot <mode> | settings | set <setting> <value> | version]".into()); 1 }
    }
}

/// `vr-host run`: keeps the host running. It starts `vr-host host` (from this program's path, so an update that swapped
/// the file starts the new version), starts it again right away after exit 3 (new code, an update, a restart asked for)
/// and 5 s after a crash, waits 30 s and tries again while the settings are missing or wrong, and ends on exit 0 / 2.
/// `stop`: set when the Windows service is asked to stop.
pub fn supervise(f: &Folder, stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> i32 {
    use std::sync::atomic::Ordering;
    let exe = std::env::current_exe().unwrap_or_else(|_| "vr-host".into());
    let stopped = stop.unwrap_or_else(|| { let s = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)); let s2 = s.clone(); let _ = ctrlc_hook(move || s2.store(true, Ordering::SeqCst)); s });
    let mut fast_fails = 0u32;
    loop {
        if stopped.load(Ordering::SeqCst) { return 0; }
        let started = std::time::Instant::now();
        let mut c = std::process::Command::new(&exe);
        c.args(["host", "--config", &f.config.display().to_string()]).current_dir(&f.dir);
        // no terminal (the Windows service, a background start): the host's lines go to ai-node.log, kept under 5 MB
        // (systemd units send it there themselves)
        if !std::io::IsTerminal::is_terminal(&std::io::stdout()) && std::env::var_os("INVOCATION_ID").is_none() {
            if std::fs::metadata(f.log()).map(|m| m.len() > 5 << 20).unwrap_or(false) { let _ = std::fs::rename(f.log(), f.f("ai-node.log.1")); }
            if let Ok(log) = std::fs::OpenOptions::new().create(true).append(true).open(f.log()) { if let Ok(l2) = log.try_clone() { c.stdout(log).stderr(l2); } }
        }
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; c.creation_flags(winsys::CREATE_NO_WINDOW); }
        let mut child = match c.spawn() { Ok(ch) => ch, Err(e) => { eprintln!("[vr-host] could not start the host: {e}"); std::thread::sleep(std::time::Duration::from_secs(5)); continue; } };
        let code = loop {
            if let Ok(Some(s)) = child.try_wait() { break s.code().unwrap_or(1); }
            if stopped.load(Ordering::SeqCst) { let _ = std::fs::write(f.stop_now(), "now"); for _ in 0..50 { if let Ok(Some(_)) = child.try_wait() { break; } std::thread::sleep(std::time::Duration::from_millis(100)); } let _ = child.kill(); let _ = child.wait(); return 0; }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        match code {
            0 | 2 => return 0,
            3 => { fast_fails = 0; continue; }
            1 => { eprintln!("[vr-host] the host cannot start with these settings; trying again in 30 s"); sleep_unless(&stopped, 30); }
            _ => {
                fast_fails = if started.elapsed().as_secs() < 30 { fast_fails + 1 } else { 0 };
                let wait = (5 * (1 + fast_fails.min(11))) as u64; // 5 s, 10 s … at most a minute when it keeps failing at once
                eprintln!("[vr-host] the host stopped (code {code}); starting it again in {wait} s");
                sleep_unless(&stopped, wait);
            }
        }
    }
}
fn sleep_unless(stop: &std::sync::atomic::AtomicBool, secs: u64) { for _ in 0..secs * 5 { if stop.load(std::sync::atomic::Ordering::SeqCst) { return; } std::thread::sleep(std::time::Duration::from_millis(200)); } }
/// Ctrl+C / SIGTERM set the flag (the supervisor then stops the host gently).
fn ctrlc_hook(f: impl Fn() + Send + 'static) -> std::io::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    std::thread::spawn(move || rt.block_on(async move {
        #[cfg(unix)]
        { use tokio::signal::unix::{signal, SignalKind}; let mut t = signal(SignalKind::terminate()).expect("signal"); tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = t.recv() => {} } }
        #[cfg(not(unix))]
        { let _ = tokio::signal::ctrl_c().await; }
        f();
    }));
    Ok(())
}
