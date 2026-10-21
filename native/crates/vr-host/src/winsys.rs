//! The few Windows calls the host needs that the standard library has no word for.
#![cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

pub fn pid_alive(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() { return false; }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}
pub fn is_admin() -> bool { unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 } }
fn wide(s: &str) -> Vec<u16> { std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect() }
/// Starts `exe args` with administrator rights (Windows asks the person once). True when it was started.
pub fn run_elevated(exe: &str, args: &str) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (verb, file, params) = (wide("runas"), wide(exe), wide(args));
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), SW_SHOWNORMAL) };
    (r as isize) > 32
}
/// The CREATE_NO_WINDOW process flag: a child that gets no console window of its own.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
