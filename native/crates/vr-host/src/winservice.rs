//! The VibeRollerAIHost Windows service: the Service Control Manager starts `vr-host service-run --config <file>`;
//! it reports running and runs the supervisor (main.rs supervise) until the SCM asks it to stop.
#![cfg(windows)]
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::files::Folder;

static FOLDER: OnceLock<Folder> = OnceLock::new();
define_windows_service!(ffi_service_main, service_main);

pub fn dispatch(folder: Folder) -> i32 {
    let _ = FOLDER.set(folder.clone());
    match service_dispatcher::start(crate::control::SVC, ffi_service_main) {
        Ok(()) => 0,
        Err(_) => crate::supervise(&folder, None), // not started by the SCM (run by hand): just supervise
    }
}
fn service_main(_args: Vec<OsString>) {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let handler = move |c| match c {
        ServiceControl::Stop | ServiceControl::Shutdown => { s2.store(true, Ordering::SeqCst); ServiceControlHandlerResult::NoError }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(h) = service_control_handler::register(crate::control::SVC, handler) else { return };
    let status = |state, accept| ServiceStatus { service_type: ServiceType::OWN_PROCESS, current_state: state, controls_accepted: accept, exit_code: ServiceExitCode::Win32(0), checkpoint: 0, wait_hint: Duration::from_secs(10), process_id: None };
    let _ = h.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN));
    let folder = FOLDER.get().cloned().unwrap_or_else(|| Folder::find(None));
    // the host's output goes to ai-node.log (a service has no console)
    let _ = std::env::set_current_dir(&folder.dir);
    crate::supervise(&folder, Some(stop));
    let _ = h.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()));
}
