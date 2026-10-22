//! Notices: what a library has to tell the people running the program (a data file that could not be read and was
//! moved aside, [`crate::json::read_json`]). A program drains them where it can show them (VibeRoller's hub posts them
//! to the team chat as "VibeRoller", every 10 s); until then they wait here. Nothing is ever lost or repeated.
use std::sync::Mutex;

static NOTICES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Something the people running this program should hear (markdown; one or two sentences).
pub fn push(text: impl Into<String>) {
    let text = text.into();
    eprintln!("[notice] {text}");
    NOTICES.lock().unwrap_or_else(|e| e.into_inner()).push(text);
}
/// Everything said since the last drain, oldest first.
pub fn drain() -> Vec<String> { std::mem::take(&mut *NOTICES.lock().unwrap_or_else(|e| e.into_inner())) }
