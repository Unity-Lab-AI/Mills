//! vr-hub: the VibeRoller hub in Rust, growing module by module in front of the JavaScript hub (bridge/server.js).
//!
//! It takes the hub's port (what nginx sends the portal to) and the JavaScript hub moves behind it, on a port of its own
//! on 127.0.0.1 (bridge/run.js starts both when config "rustHub" is on, and falls back to the JavaScript hub alone if
//! this one is missing or keeps failing, so nobody is ever cut off). Every request this program has a module for is
//! answered here; everything else passes through to the JavaScript hub, streamed both ways (long polls, streamed
//! answers, uploads). While the JavaScript hub restarts (a deploy, new code), requests wait for it instead of failing.
//!   vr-hub --port <front> --back <port of the JavaScript hub> --root <bridge folder>
//!   vr-hub --port <front> --alone --root <bridge folder>          (5.6.0: no JavaScript hub; what it has no module for is 404)
//! The regression suite runs every check through it (node bridge/test/regress.js --rust-hub).
mod front;
mod modules;

use std::net::SocketAddr;
use std::path::PathBuf;

pub const VERSION: &str = match option_env!("VR_VERSION") { Some(v) => v, None => env!("CARGO_PKG_VERSION") };

fn arg(args: &[String], k: &str) -> Option<String> { args.iter().position(|a| a == k).and_then(|i| args.get(i + 1).cloned()) }

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--selftest") { println!("vr-hub {VERSION} loads"); return; }
    if args.iter().any(|a| a == "--version") { println!("{VERSION}"); return; }
    // which modules it answers (bridge/run.js tells the JavaScript hub, which then leaves them alone)
    if args.iter().any(|a| a == "--modules") { println!("{}", modules::native_modules().join(",")); return; }
    let root = PathBuf::from(arg(&args, "--root").unwrap_or_else(|| ".".into()));
    let cfg = modules::Config::load(&root);
    let port: u16 = arg(&args, "--port").and_then(|p| p.parse().ok()).unwrap_or(cfg.port);
    // --alone (FR6, 5.6.0): no JavaScript hub behind this one; else --back names its port
    let alone = args.iter().any(|a| a == "--alone");
    let back: Option<u16> = match arg(&args, "--back").and_then(|p| p.parse().ok()) { Some(b) => Some(b), None if alone => None, None => { eprintln!("[vr-hub] --back <port of the JavaScript hub> is needed (or --alone)"); std::process::exit(1); } };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio");
    let code = rt.block_on(async move {
        let hub = match front::Hub::new(root, cfg, if alone { None } else { back.map(|b| SocketAddr::from(([127, 0, 0, 1], b))) }) { Ok(h) => h, Err(e) => { eprintln!("[vr-hub] {e}"); return 1; } };
        front::serve(hub, SocketAddr::from(([127, 0, 0, 1], port))).await
    });
    // what the write buffer holds goes to disk before the process does (T-331: a graceful restart loses nothing)
    if !pgm_core_internal::store::global().shutdown() { eprintln!("[vr-hub] store: not everything was written within 10 s"); }
    std::process::exit(code);
}
