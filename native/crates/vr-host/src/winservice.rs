//! The VibeRollerAIHost Windows service: the Service Control Manager starts `vr-host service-run --config <file>`;
//! it reports running and runs the supervisor (main.rs supervise) until the SCM asks it to stop (the SCM part:
//! pgm-platform-internal winsvc since 5.0.0).
#![cfg(windows)]
use crate::files::Folder;

pub fn dispatch(folder: Folder) -> i32 {
    let f2 = folder.clone();
    pgm_platform_internal::winsvc::dispatch(crate::control::SVC, move |stop| {
        // the host's output goes to ai-node.log (a service has no console)
        let _ = std::env::set_current_dir(&f2.dir);
        crate::supervise(&f2, Some(stop));
    }, || crate::supervise(&folder, None)) // not started by the SCM (run by hand): just supervise
}
