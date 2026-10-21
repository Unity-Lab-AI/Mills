//! vr-hostw: starts vr-host (next to it) with the same arguments and no window at all: what "start at sign-in" runs on
//! Windows, so no console flashes up. vr-host itself is a console program on purpose: run from a terminal it uses that
//! terminal, double-clicked it opens one for its dashboard.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    let me = std::env::current_exe().unwrap_or_default();
    let host = me.with_file_name(if cfg!(windows) { "vr-host.exe" } else { "vr-host" });
    let mut c = std::process::Command::new(host);
    c.args(std::env::args_os().skip(1)).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000 /* CREATE_NO_WINDOW */); }
    // waits, so whoever started it (the sign-in) sees one process for as long as the host runs
    std::process::exit(c.status().ok().and_then(|s| s.code()).unwrap_or(1));
}
