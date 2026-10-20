//! vr-hub: the VibeRoller hub in Rust, growing module by module in front of the JavaScript hub (bridge/server.js).
//!
//! It takes the hub's port (what nginx sends the portal to) and the JavaScript hub moves behind it, on a port of its own
//! on 127.0.0.1 (bridge/run.js starts both when config "rustHub" is on, and falls back to the JavaScript hub alone if
//! this one is missing or keeps failing, so nobody is ever cut off). Every request this program has a module for is
//! answered here; everything else passes through to the JavaScript hub, streamed both ways (long polls, streamed
//! answers, uploads). While the JavaScript hub restarts (a deploy, new code), requests wait for it instead of failing.
//!   vr-hub --port <front> --back <port of the JavaScript hub> --root <bridge folder>
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
    let root = PathBuf::from(arg(&args, "--root").unwrap_or_else(|| ".".into()));
    let cfg = modules::Config::load(&root);
    let port: u16 = arg(&args, "--port").and_then(|p| p.parse().ok()).unwrap_or(cfg.port);
    let back: u16 = match arg(&args, "--back").and_then(|p| p.parse().ok()) { Some(b) => b, None => { eprintln!("[vr-hub] --back <port of the JavaScript hub> is needed"); std::process::exit(1); } };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio");
    let code = rt.block_on(async move {
        let hub = match front::Hub::new(root, cfg, SocketAddr::from(([127, 0, 0, 1], back))) { Ok(h) => h, Err(e) => { eprintln!("[vr-hub] {e}"); return 1; } };
        front::serve(hub, SocketAddr::from(([127, 0, 0, 1], port))).await
    });
    std::process::exit(code);
}
