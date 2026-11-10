//! vr-run: VibeRoller's supervisor in Rust (FR4, team note plans/full-rust-hub.md; R177). What bridge/run.js did, with
//! what it could not do: a hub that hangs is noticed, and a build that is broken gives way to the previous good one.
//!
//!   vr-run --root <bridge> --port <p> --builds <native/dist/<target>> [--health-every 10] [--health-timeout 5]
//!          [--retry-after 600] [--back <port>]
//!
//! - The kept builds (native/keep-builds.sh): `<builds>/current` names the newest build, `<builds>/<commit>/vr-hub` is
//!   its binary, `<builds>/good` lists the good builds newest first (the auto-deploy writes it when a version is
//!   verified). vr-run starts the newest build; the JavaScript hub behind it (bridge/server.js, with VR_NODE or `node`)
//!   as long as that file exists (FR6 removes it).
//! - Health: every `--health-every` s GET /api/status from this machine, 200 within `--health-timeout` s. 3 misses in a
//!   row: the build is dead or hung and is killed. Exit 75 = restart on new code (`current` is read again). Exit 0 =
//!   stop. Anything else is a crash: started again after a second.
//! - Fallback: 3 crashes within a minute, or 3 health misses in a row, mark the build bad: the previous good build
//!   starts (then the one before). `<root>/.fell-back` says {from, to, why, at} and `<root>/.announce/` carries a line
//!   the hub posts to the team chat. The newest build is tried again after `--retry-after` s (then every hour): a fix
//!   that lands reaches people without anyone restarting anything.
//! - The order of service (T-157; `decide`, `serving_label`): the newest build ("rust"), then the previous good builds
//!   ("rust-fallback-1", "rust-fallback-2": native/keep-builds.sh keeps the current + 2 good ones), and only when every
//!   Rust build fails the frozen JavaScript hub alone ("javascript-fallback", kept even with `--alone`). Each hub gets
//!   VR_SERVING and shows it as `serving` in /api/status from this machine.
//! - Ctrl-C / SIGTERM stop everything (exit 0). systemd keeps vr-run itself alive (deploy/viberoller-hub.service).
//! Windows and Linux alike (std::process; no shell).
use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const TAG: &str = "[vr-run]";

#[derive(Clone)]
struct Opts { root: PathBuf, port: u16, back: u16, builds: PathBuf, health_every: u64, health_timeout: u64, retry_after: u64, alone: bool }

fn arg(args: &[String], k: &str) -> Option<String> { args.iter().position(|a| a == k).and_then(|i| args.get(i + 1).cloned()) }
fn now_s() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }
fn log(s: &str) { println!("{TAG} {s}"); }

/// The builds in the order to try: `current` first, then the good list (newest first), each once, existing only.
fn candidates(builds: &Path) -> Vec<String> {
    let exe = if cfg!(windows) { "vr-hub.exe" } else { "vr-hub" };
    let cur = std::fs::read_to_string(builds.join("current")).map(|s| s.trim().to_string()).unwrap_or_default();
    let mut out: Vec<String> = vec![];
    for c in std::iter::once(cur).chain(std::fs::read_to_string(builds.join("good")).unwrap_or_default().lines().map(|l| l.trim().to_string())) {
        if c.is_empty() || out.contains(&c) || !builds.join(&c).join(exe).is_file() { continue; }
        out.push(c);
    }
    out
}

/// What serves the port. Rust(i) = the i-th candidate build (0 = the newest, 1 and 2 = the previous good builds);
/// Javascript = the frozen last resort; Nothing = no build and no JavaScript hub (vr-run keeps trying).
#[derive(Debug, PartialEq, Clone)]
enum Serving { Rust(usize), Javascript, Nothing }

/// The name /api/status shows as `serving`.
fn serving_label(s: &Serving) -> String {
    match s { Serving::Rust(0) => "rust".into(), Serving::Rust(i) => format!("rust-fallback-{i}"), Serving::Javascript => "javascript-fallback".into(), Serving::Nothing => "none".into() }
}

/// The decision order, pure: the first candidate build that starts serves; when none does, the JavaScript hub (if it
/// can run at all); else nothing. `starts` says whether a build starts and passes its health check.
fn decide(cands: &[String], starts: &dyn Fn(&str) -> bool, js_available: bool) -> Serving {
    for (i, c) in cands.iter().enumerate() { if starts(c) { return Serving::Rust(i); } }
    if js_available { Serving::Javascript } else { Serving::Nothing }
}

/// The JavaScript hub behind the Rust one (while bridge/server.js exists): started with what run.js gave it; started
/// again when it exits (75 = its own restart; anything else after 5 s).
struct Back { child: Option<Child>, node: Option<PathBuf>, script: PathBuf, secret: String, port: u16, behind: bool }
impl Back {
    fn new(root: &Path, back: u16, secret: &str, behind: bool) -> Back {
        let script = root.join("server.js");
        let node = std::env::var_os("VR_NODE").map(PathBuf::from).filter(|p| p.is_file()).or_else(|| which("node"));
        Back { child: None, node: if script.is_file() { node } else { None }, script, secret: secret.into(), port: back, behind }
    }
    /// Is the JavaScript hub run behind the Rust one? (Not when the Rust hub serves alone.)
    fn wanted(&self) -> bool { self.behind && self.node.is_some() }
    /// Can the JavaScript hub take the port alone, as the last resort? (Alone mode keeps it for exactly this.)
    fn can_fallback(&self) -> bool { self.node.is_some() }
    async fn start(&mut self, mods: &str) {
        if !self.behind { return; }
        let Some(node) = self.node.clone() else { return };
        self.stop().await;
        let mut c = { #[allow(unused_mut)] let mut c = Command::new(&node); #[cfg(windows)] { c.creation_flags(0x0800_0000); } /* CREATE_NO_WINDOW */ c };
        c.arg(&self.script).arg("--no-tunnel").current_dir(self.script.parent().unwrap_or(Path::new(".")))
            .env("MILLS_SUPERVISED", "1").env("VR_BACK_PORT", self.port.to_string()).env("VR_FRONT_SECRET", &self.secret).env("VR_FRONT_MODULES", mods)
            .stdin(Stdio::null()).kill_on_drop(true);
        match c.spawn() { Ok(ch) => { log(&format!("the JavaScript hub behind on 127.0.0.1:{} (pid {})", self.port, ch.id().unwrap_or(0))); self.child = Some(ch); } Err(e) => log(&format!("could not start the JavaScript hub: {e}")) }
    }
    async fn stop(&mut self) { if let Some(mut ch) = self.child.take() { let _ = ch.kill().await; } }
    /// The JavaScript hub alone on the front port (no Rust hub in front): what run.js did when the Rust hub kept failing.
    async fn start_alone(&mut self) {
        let Some(node) = self.node.clone() else { return };
        self.stop().await;
        let mut c = { #[allow(unused_mut)] let mut c = Command::new(&node); #[cfg(windows)] { c.creation_flags(0x0800_0000); } /* CREATE_NO_WINDOW */ c };
        c.arg(&self.script).arg("--no-tunnel").current_dir(self.script.parent().unwrap_or(Path::new("."))).env("MILLS_SUPERVISED", "1").env("MILLS_PORT", self.port.to_string()).env("VR_SERVING", serving_label(&Serving::Javascript)).stdin(Stdio::null()).kill_on_drop(true);
        match c.spawn() { Ok(ch) => { log(&format!("the JavaScript hub alone on port {} (pid {})", self.port, ch.id().unwrap_or(0))); self.child = Some(ch); } Err(e) => log(&format!("could not start the JavaScript hub alone: {e}")) }
    }
    /// Started again if it is gone (run.js: 75 at once, a crash after 5 s).
    async fn keep(&mut self, mods: &str) {
        let exited: Option<i32> = match self.child.as_mut() {
            None => None,
            Some(ch) => match ch.try_wait() { Ok(Some(st)) => Some(st.code().unwrap_or(-1)), _ => return },
        };
        match exited {
            None => { if self.wanted() { self.start(mods).await; } }
            Some(code) => {
                log(&format!("the JavaScript hub exited with {code}; starting it again{}", if code == 75 { "" } else { " in 5 s" }));
                if code != 75 { tokio::time::sleep(Duration::from_secs(5)).await; }
                self.start(mods).await;
            }
        }
    }
}
fn which(name: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) { vec![format!("{name}.exe"), format!("{name}.cmd"), name.into()] } else { vec![name.into()] };
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}
fn modules_of(bin: &Path) -> String { { #[allow(unused_mut)] let mut c = std::process::Command::new(bin); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.arg("--modules").output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default() }

/// GET /api/status on this machine: 200 within the timeout, or why not.
async fn health(port: u16, timeout: u64) -> Result<(), String> {
    let req = async {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| format!("connect: {e}"))?;
        s.write_all(b"GET /api/status HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").await.map_err(|e| format!("send: {e}"))?;
        let mut buf = Vec::new(); s.read_to_end(&mut buf).await.map_err(|e| format!("read: {e}"))?;
        let head = String::from_utf8_lossy(&buf);
        let status = head.split_whitespace().nth(1).unwrap_or("").to_string();
        if status == "200" { Ok(()) } else { Err(format!("answered {}", if status.is_empty() { "nothing" } else { &status })) }
    };
    tokio::time::timeout(Duration::from_secs(timeout), req).await.unwrap_or_else(|_| Err(format!("no answer within {timeout} s")))
}

enum Outcome { Restart, Stop, Bad(String), Retry }

/// One build in service until it asks for a restart, stops, or proves bad.
async fn run_build(o: &Opts, commit: &str, idx: usize, back: Arc<Mutex<Back>>, stop: &tokio::sync::watch::Receiver<bool>) -> Outcome {
    let exe = o.builds.join(commit).join(if cfg!(windows) { "vr-hub.exe" } else { "vr-hub" });
    let mods = modules_of(&exe);
    let secret = back.lock().await.secret.clone();
    let is_newest = idx == 0;
    let serving = serving_label(&Serving::Rust(idx));
    if !is_newest { log(&format!("SERVING {serving}: build {commit} (an older good build; the newer ones failed)")); }
    let mut crashes: VecDeque<Instant> = VecDeque::new();
    let started_fallback = Instant::now();
    loop {
        let mut c = { #[allow(unused_mut)] let mut c = Command::new(&exe); #[cfg(windows)] { c.creation_flags(0x0800_0000); } /* CREATE_NO_WINDOW */ c };
        c.args(["--port", &o.port.to_string()]);
        if o.alone { c.arg("--alone"); } else { c.args(["--back", &o.back.to_string()]); }
        c.args(["--root", &o.root.display().to_string()]).env("MILLS_SUPERVISED", "1").env("VR_FRONT_SECRET", &secret).env("VR_SERVING", &serving).stdin(Stdio::null()).kill_on_drop(true);
        if let Some(n) = std::env::var_os("VR_NODE").or_else(|| which("node").map(|p| p.into_os_string())) { c.env("VR_NODE", n); }
        let mut child = match c.spawn() { Ok(ch) => ch, Err(e) => return Outcome::Bad(format!("could not start {}: {e}", exe.display())) };
        log(&format!("vr-hub build {} on 127.0.0.1:{} (pid {})", commit, o.port, child.id().unwrap_or(0)));
        back.lock().await.start(&mods).await;
        let mut misses = 0u32;
        let mut tick = tokio::time::interval(Duration::from_secs(o.health_every.max(1)));
        tick.tick().await; // the first tick is now
        let mut stop = stop.clone();
        let exit = loop {
            tokio::select! {
                _ = stop.changed() => { let _ = child.kill().await; back.lock().await.stop().await; return Outcome::Stop; }
                st = child.wait() => break st.ok().and_then(|s| s.code()).unwrap_or(-1),
                _ = tick.tick() => {
                    match health(o.port, o.health_timeout.max(1)).await {
                        Ok(()) => misses = 0,
                        Err(why) => { misses += 1; log(&format!("health check {misses}/3 missed on build {commit}: {why}")); if misses >= 3 { let _ = child.kill().await; return Outcome::Bad("no answer to 3 health checks in a row (hung or dead)".into()); } }
                    }
                    back.lock().await.keep(&mods).await;
                    if !is_newest && started_fallback.elapsed() >= Duration::from_secs(o.retry_after.max(1)) { let _ = child.kill().await; return Outcome::Retry; }
                }
            }
        };
        match exit {
            0 => { back.lock().await.stop().await; return Outcome::Stop; }
            75 => { log("restart asked for (new code): reading the builds again"); return Outcome::Restart; }
            code => {
                let now = Instant::now(); crashes.push_back(now); while crashes.front().map(|t| now.duration_since(*t) > Duration::from_secs(60)).unwrap_or(false) { crashes.pop_front(); }
                if crashes.len() >= 3 { return Outcome::Bad(format!("crashed {} times within a minute (last exit code {code})", crashes.len())); }
                log(&format!("vr-hub build {commit} exited with {code}; starting it again in a second"));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Does bridge/'s config say the Rust hub serves alone ("rustHub": { "alone": true }; config.local.json before config.json)?
fn alone_in_config(root: &Path) -> bool {
    pgm_core_internal::json::read_json(&root.join("config.local.json")).or_else(|| pgm_core_internal::json::read_json(&root.join("config.json"))).map(|c| c["rustHub"]["alone"] == json!(true)).unwrap_or(false)
}

fn fell_back(o: &Opts, from: &str, to: &str, why: &str) {
    let at = now_s();
    pgm_core_internal::json::write_json(&o.root.join(".fell-back"), &json!({ "from": from, "to": to, "why": why, "at": at }));
    let dir = o.root.join(".announce"); let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("fell-back-{at}.txt")), format!("**VibeRoller fell back to an earlier build.** The build {} {why}, so the hub runs the previous good build {} now. Nothing was lost. The newest build is tried again after {} s; whoever shipped it: look at the server's log, fix, and ship again.\n", &from[..from.len().min(7)], &to[..to.len().min(7)], o.retry_after));
    log(&format!("FELL BACK: build {from} {why}; running {to}"));
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") { println!("vr-run {}", env!("CARGO_PKG_VERSION")); return; }
    let Some(root) = arg(&args, "--root").map(PathBuf::from) else { eprintln!("usage: vr-run --root <bridge> --port <p> --builds <dist/target> [--health-every 10] [--health-timeout 5] [--retry-after 600] [--back <port>] [--alone]"); std::process::exit(2); };
    let port: u16 = arg(&args, "--port").and_then(|p| p.parse().ok()).unwrap_or(7777);
    // --alone (FR6): no JavaScript hub behind, whatever bridge/ holds (config "rustHub": { "alone": true } says the same;
    // read again at every restart a build asks for, so the swap is the config line and a graceful restart, R195)
    let alone_arg = args.iter().any(|a| a == "--alone");
    let alone = alone_arg || alone_in_config(&root);
    let mut opts = Opts {
        root: std::path::absolute(&root).unwrap_or(root), port, alone,
        back: arg(&args, "--back").and_then(|p| p.parse().ok()).unwrap_or(port + 2),
        builds: arg(&args, "--builds").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("native/dist")),
        health_every: arg(&args, "--health-every").and_then(|p| p.parse().ok()).unwrap_or(10),
        health_timeout: arg(&args, "--health-timeout").and_then(|p| p.parse().ok()).unwrap_or(5),
        retry_after: arg(&args, "--retry-after").and_then(|p| p.parse().ok()).unwrap_or(600),
    };
    let secret = pgm_core_internal::crypto::random_hex(24);
    // alone: the JavaScript hub is not run behind the Rust one, but it stays known as the last resort (bridge/server.js)
    let b = Back::new(&opts.root, opts.back, &secret, !opts.alone);
    let back = Arc::new(Mutex::new(b));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        #[cfg(unix)]
        { let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal"); tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} } }
        #[cfg(not(unix))]
        { let _ = tokio::signal::ctrl_c().await; }
        let _ = stop_tx.send(true);
    });
    let mut bad: Vec<String> = vec![];
    let mut retries: u32 = 0;
    'chain: loop {
        // after the first retry the newest build is only tried hourly: a fallback that works is not bounced every 10 minutes
        let o = { let mut oo = opts.clone(); if retries > 1 { oo.retry_after = oo.retry_after.max(3600); } oo };
        let cands = candidates(&o.builds);
        if cands.is_empty() && !back.lock().await.can_fallback() { log(&format!("no build under {} (native/build.sh makes one); waiting", o.builds.display())); tokio::time::sleep(Duration::from_secs(10)).await; if *stop_rx.borrow() { return; } continue; }
        let newest = cands.first().cloned().unwrap_or_default();
        let mut ran_any = false;
        for (i, c) in cands.iter().enumerate() {
            if bad.contains(c) { continue; }
            ran_any = true;
            let next = cands.iter().skip(i + 1).find(|x| !bad.contains(x)).cloned();
            match run_build(&o, c, i, back.clone(), &stop_rx).await {
                Outcome::Stop => { log("stopping"); return; }
                Outcome::Restart => {
                    let now_alone = alone_arg || alone_in_config(&o.root);
                    if now_alone != o.alone {
                        log(&format!("rustHub.alone is now {now_alone}: {}", if now_alone { "the Rust hub serves alone (the JavaScript hub stays the last resort)" } else { "the JavaScript hub runs behind the Rust one again" }));
                        opts.alone = now_alone;
                        let mut b = back.lock().await; b.behind = !now_alone; if now_alone { b.stop().await; }
                    }
                    bad.clear(); retries = 0; continue 'chain;
                }
                Outcome::Retry => { log(&format!("trying the newest build {newest} again")); bad.clear(); retries += 1; continue 'chain; }
                Outcome::Bad(why) => {
                    bad.push(c.clone());
                    match &next { Some(n) => fell_back(&o, c, n, &why), None => log(&format!("build {c} {why}; no other build to fall back to")) }
                }
            }
        }
        if !ran_any || cands.iter().all(|c| bad.contains(c)) {
            // every kept Rust build failed (or none exists). The JavaScript hub (bridge/server.js, frozen as the last
            // resort and kept even when the Rust hub serves alone) takes the port alone until the newest build is
            // tried again. It is said loudly: in this log, in the team chat, and in /api/status ("serving").
            let wait = if retries == 0 { o.retry_after.min(600) } else { 3600 };
            if back.lock().await.can_fallback() {
                log(&format!("!!! SERVING javascript-fallback: every kept Rust build failed; the JavaScript hub takes port {} alone (the last resort); the newest build is tried again in {wait} s", o.port));
                let dir = o.root.join(".announce"); let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(dir.join(format!("js-alone-{}.txt", now_s())), format!("**VibeRoller runs on its JavaScript hub alone (the last fallback).** Every kept Rust build failed ({}), so the frozen JavaScript hub took the port by itself. The basics work (sign-in, chat, the board, Work with Claude, the AI host link); newer features need the Rust hub. The newest Rust build is tried again in {wait} s. Whoever shipped it: look at the server's log, fix, and ship again.\n", cands.iter().map(|c| c[..c.len().min(7)].to_string()).collect::<Vec<_>>().join(", ")));
                let mut b = back.lock().await;
                b.port = o.port; b.start_alone().await;
                let deadline = Instant::now() + Duration::from_secs(wait);
                let mut stopped = false;
                while Instant::now() < deadline {
                    let gone = tokio::select! { _ = tokio::time::sleep(Duration::from_secs(1)) => false, _ = async { let mut s = stop_rx.clone(); let _ = s.changed().await; } => { stopped = true; true } };
                    if gone { break; }
                    // the JavaScript hub keeps itself up here: 75 (a restart was asked for) means new code may be waiting, so the
                    // chain is tried again at once; a crash is started again after 5 s
                    let code = match b.child.as_mut() { Some(ch) => ch.try_wait().ok().flatten().map(|s| s.code().unwrap_or(-1)), None => Some(-1) };
                    if let Some(code) = code {
                        if code == 75 { log("the JavaScript hub asked for a restart: trying the Rust builds again"); break; }
                        log(&format!("the JavaScript hub (last resort) exited with {code}; starting it again in 5 s"));
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        b.start_alone().await;
                    }
                }
                b.stop().await; b.port = o.back;
                if stopped { return; }
            } else {
                log(&format!("every kept build failed; trying again in {wait} s"));
                tokio::select! { _ = tokio::time::sleep(Duration::from_secs(wait)) => {}, _ = async { let mut s = stop_rx.clone(); let _ = s.changed().await; } => { return; } }
            }
            bad.clear(); retries += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(a: &[&str]) -> Vec<String> { a.iter().map(|s| s.to_string()).collect() }
    #[test]
    fn the_newest_build_serves_when_it_starts() { assert_eq!(decide(&v(&["a", "b", "c"]), &|_| true, true), Serving::Rust(0)); }
    #[test]
    fn the_previous_good_builds_come_in_order() {
        assert_eq!(decide(&v(&["a", "b", "c"]), &|c| c != "a", true), Serving::Rust(1));
        assert_eq!(decide(&v(&["a", "b", "c"]), &|c| c == "c", true), Serving::Rust(2));
    }
    #[test]
    fn javascript_only_when_every_rust_build_fails() {
        assert_eq!(decide(&v(&["a", "b", "c"]), &|_| false, true), Serving::Javascript);
        assert_eq!(decide(&[], &|_| true, true), Serving::Javascript);
        assert_eq!(decide(&v(&["a"]), &|_| false, false), Serving::Nothing);
    }
    #[test]
    fn the_labels_status_shows() {
        assert_eq!(serving_label(&Serving::Rust(0)), "rust");
        assert_eq!(serving_label(&Serving::Rust(1)), "rust-fallback-1");
        assert_eq!(serving_label(&Serving::Rust(2)), "rust-fallback-2");
        assert_eq!(serving_label(&Serving::Javascript), "javascript-fallback");
    }
}
