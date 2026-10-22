//! Windows: running inside the Service Control Manager. [`dispatch`] hands the program to the SCM under its service
//! name; the SCM calls back, the service reports running and `body` runs until the SCM asks it to stop (the flag it
//! gets turns true), then it reports stopped. Run by hand (not by the SCM), `fallback` runs instead.
#![cfg(windows)]
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

type Body = Box<dyn Fn(Arc<AtomicBool>) + Send + Sync>;
static NAME: OnceLock<String> = OnceLock::new();
static BODY: OnceLock<Body> = OnceLock::new();
define_windows_service!(ffi_service_main, service_main);

/// Runs `body` as the service `name` (blocks until the service stopped). When the SCM did not start this program,
/// `fallback()` runs and its exit code is returned.
pub fn dispatch(name: &str, body: impl Fn(Arc<AtomicBool>) + Send + Sync + 'static, fallback: impl FnOnce() -> i32) -> i32 {
    let _ = NAME.set(name.to_string());
    let _ = BODY.set(Box::new(body));
    match service_dispatcher::start(name, ffi_service_main) { Ok(()) => 0, Err(_) => fallback() }
}
fn service_main(_args: Vec<OsString>) {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let handler = move |c| match c {
        ServiceControl::Stop | ServiceControl::Shutdown => { s2.store(true, Ordering::SeqCst); ServiceControlHandlerResult::NoError }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(h) = service_control_handler::register(NAME.get().map(String::as_str).unwrap_or(""), handler) else { return };
    let status = |state, accept| ServiceStatus { service_type: ServiceType::OWN_PROCESS, current_state: state, controls_accepted: accept, exit_code: ServiceExitCode::Win32(0), checkpoint: 0, wait_hint: Duration::from_secs(10), process_id: None };
    let _ = h.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN));
    if let Some(body) = BODY.get() { body(stop); }
    let _ = h.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()));
}
