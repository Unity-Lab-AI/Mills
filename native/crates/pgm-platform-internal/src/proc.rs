//! Starting other programs the same way on Windows and Linux: found on the PATH (on Windows with PATHEXT, so npm shims
//! like claude.cmd are found; Rust quotes arguments for .cmd files safely), never with a window of their own; is a
//! process alive; stopping one; administrator rights on Windows; a detached background start.
use std::path::{Path, PathBuf};

/// The CREATE_NO_WINDOW process flag: a child that gets no console window of its own.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The full path of a program on the PATH, or None.
pub fn which(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(name);
    if p.components().count() > 1 || p.is_absolute() { return if p.exists() { Some(p) } else { None }; }
    let exts: Vec<String> = if cfg!(windows) { std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into()).split(';').map(|s| s.to_ascii_lowercase()).collect() } else { vec![String::new()] };
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for ext in &exts {
            let f = dir.join(format!("{name}{ext}"));
            if f.is_file() { return Some(f); }
        }
        if cfg!(windows) && dir.join(name).is_file() && name.contains('.') { return Some(dir.join(name)); }
    }
    None
}
/// A tokio Command for `name` (resolved on the PATH), with no console window on Windows, killed when dropped.
pub fn command(name: &str) -> tokio::process::Command {
    let mut c = { #[allow(unused_mut)] let mut c = tokio::process::Command::new(which(name).unwrap_or_else(|| PathBuf::from(name))); #[cfg(windows)] { c.creation_flags(0x0800_0000); } /* CREATE_NO_WINDOW */ c };
    #[cfg(windows)]
    { c.creation_flags(CREATE_NO_WINDOW); }
    c.kill_on_drop(true);
    c
}
/// The same for a blocking std Command.
pub fn std_command(name: &str) -> std::process::Command {
    #[allow(unused_mut)] // only Windows changes it
    let mut c = { #[allow(unused_mut)] let mut c = std::process::Command::new(which(name).unwrap_or_else(|| PathBuf::from(name))); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c };
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(CREATE_NO_WINDOW); }
    c
}
/// Runs a program and gives (success, stdout + stderr).
pub fn run(name: &str, args: &[&str]) -> (bool, String) {
    match std_command(name).args(args).output() {
        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (false, e.to_string()),
    }
}

/// Is that process still there? (Unix: kill(pid, 0), also when it belongs to someone else; Windows: its exit code.)
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0): 0 = it exists; EPERM = it exists but belongs to someone else
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() { return false; }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }
}
/// Asks a process to stop (Unix: SIGTERM; Windows: taskkill of it and its children).
#[cfg(unix)]
pub fn kill_pid(pid: u32) { unsafe { libc::kill(pid as i32, libc::SIGTERM); } }
#[cfg(not(unix))]
pub fn kill_pid(pid: u32) { let _ = { #[allow(unused_mut)] let mut c = std::process::Command::new("taskkill"); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.args(["/pid", &pid.to_string(), "/T", "/F"]).spawn(); }

/// Windows: does this program run with administrator rights?
#[cfg(windows)]
pub fn is_admin() -> bool { unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 } }
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> { use std::os::windows::ffi::OsStrExt; std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect() }
/// Windows: starts `exe args` with administrator rights (Windows asks the person once). True when it was started.
#[cfg(windows)]
pub fn run_elevated(exe: &str, args: &str) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (verb, file, params) = (wide("runas"), wide(exe), wide(args));
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), SW_SHOWNORMAL) };
    (r as isize) > 32
}

/// Starts `exe args` in `dir` in the background, its output appended to `log`, no window, not ended with its parent.
pub fn spawn_detached(exe: &str, args: &[String], dir: &Path, log: &Path) -> std::io::Result<()> {
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let mut c = { #[allow(unused_mut)] let mut c = std::process::Command::new(exe); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c };
    c.args(args).current_dir(dir).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(CREATE_NO_WINDOW | 0x0000_0008 /* DETACHED_PROCESS */); }
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; c.process_group(0); }
    c.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_and_runs() {
        assert!(super::pid_alive(std::process::id()));
        if cfg!(unix) { assert!(super::which("sh").is_some()); let (ok, out) = super::run("sh", &["-c", "echo hi"]); assert!(ok && out.trim() == "hi"); }
    }
}
