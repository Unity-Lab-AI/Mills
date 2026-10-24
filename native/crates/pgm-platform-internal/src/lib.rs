//! **pgm-platform-internal** = PGM_Platform_Internal (PsyGraphMediaPlatformInternal): the machine, the same on Windows
//! and Linux. Nothing here knows about any one product: names, texts and paths come in as parameters.
//! - [`proc`]: find a program on the PATH (PATHEXT on Windows), start it with no window, run it and read its output,
//!   is a process alive, stop it, administrator rights (Windows), start a program detached in the background
//! - [`folders`]: the home folder
//! - [`path`]: a program runnable by its name from any new terminal (a link in ~/.local/bin + a marked profile block on
//!   Linux; the user Path on Windows), idempotent
//! - [`service`]: a program as a service: the Windows service (from boot, before anyone signs in), a start at sign-in
//!   (Windows Run key), the systemd system service or user service (Linux); status, start, stop, start at boot
//! - [`winsvc`] (Windows): running inside the Service Control Manager

pub mod folders;
pub mod path;
pub mod proc;
pub mod service;
#[cfg(windows)]
pub mod winsvc;

/// Is this Windows? (the same as cfg!(windows), for code that branches at run time)
pub const WIN: bool = cfg!(windows);
