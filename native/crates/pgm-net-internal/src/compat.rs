//! Compatibility between hubs, AI hosts and linked machines (T-310, plans/linked-machines.md section 9). Pure rules, no I/O:
//! the hub, vr-host and the desktop apps all ask the same functions, so "who may do what with whom" lives in one place.
//!
//! - **Older app, newer hub.** An app that does not announce the `machine` cap is not a linked machine. It is still an AI host and
//!   still serves its `pc` engine (`pc.folder`): [`pc_mode`] says "one folder (older app)". The hub never sends it a machine job.
//! - **Newer app, older hub.** An older hub's hello answer carries no `machine` object, so [`hub_offers_machine`] is false and the
//!   app stays an AI host ([`role_with`]); the link screen says the hub needs updating. The frozen JavaScript hub is such a hub.
//! - **A scope with a higher `v`** is refused whole wherever it arrives (the library's [`Scope::try_parse`]); [`scope_from_poll`] is
//!   the hub's boundary for a scope document a machine's poll carries: nothing of it is taken, the machine record keeps what it had.

use serde_json::Value;

use crate::machine::scope::{Scope, ScopeError};

/// The cap an app announces in hello when it can be a linked machine (`machine`, section 9). Not part of `hostlink::CAPS` until the
/// machine tools exist on the app (T-311): an app that does not announce it is an AI host and nothing more.
pub const CAP_MACHINE: &str = "machine";
/// The machine protocol version the hub announces in its hello answer (`"machine": {"v": 1}`).
pub const MACHINE_V: u64 = 1;

/// What a hub's hello answer says about machines: true only when it carries `machine: {v >= 1}`. An older hub (and the frozen
/// JavaScript hub) says nothing, so the answer is false.
pub fn hub_offers_machine(hello_answer: &Value) -> bool {
    hello_answer.get("machine").and_then(|m| m.get("v")).and_then(Value::as_u64).map(|v| v >= 1).unwrap_or(false)
}

/// What an app does against a hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Serves AI providers and the `pc` engine only (the way it always did).
    AiHost,
    /// Also answers machine jobs.
    Machine,
}

/// The role an app takes: it answers machine jobs only when it has the machine code AND the hub announced `machine` back.
pub fn role_with(app_has_machine: bool, hello_answer: &Value) -> Role {
    if app_has_machine && hub_offers_machine(hello_answer) { Role::Machine } else { Role::AiHost }
}

/// May the hub send this app a machine job? Only when the app announced the `machine` cap in hello.
pub fn hub_may_send_machine_job(caps: &[String]) -> bool { caps.iter().any(|c| c == CAP_MACHINE) }

/// How the hub shows a host's computer access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcMode {
    /// A linked machine (announced the `machine` cap).
    Machine,
    /// A host with a `pc.folder` and no `machine` cap: one folder, as before ("one folder (older app)").
    OneFolderOlderApp,
    /// No computer access.
    None,
}
pub fn pc_mode(caps: &[String], pc: bool) -> PcMode {
    if hub_may_send_machine_job(caps) && pc { PcMode::Machine } else if pc { PcMode::OneFolderOlderApp } else { PcMode::None }
}
impl PcMode {
    pub fn label(self) -> &'static str { match self { PcMode::Machine => "linked machine", PcMode::OneFolderOlderApp => "one folder (older app)", PcMode::None => "no computer access" } }
}

/// A scope document a machine's poll carries, read at the hub's boundary. A document with a `v` newer than the library reads (or
/// anything unreadable) is refused whole: nothing of it is used. Ok = the scope in words, made here from the parsed document (never
/// the text the machine typed beside it).
pub fn scope_from_poll(doc: &Value, now: i64) -> Result<String, String> {
    let text = match doc { Value::String(s) => s.clone(), Value::Null => return Err("no scope".into()), v => v.to_string() };
    if text.len() > 256 * 1024 { return Err("scope is too large".into()); }
    match Scope::try_parse(&text) {
        Ok(s) => Ok(s.summary(now)),
        Err(ScopeError::TooNew { found, supported }) => Err(format!("this scope needs a newer hub (scope v{found}, this hub reads up to v{supported})")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_older_hub_offers_no_machine_and_a_newer_app_stays_an_ai_host() {
        assert!(!hub_offers_machine(&json!({ "id": "h1", "hubKey": "ab" })), "an older hub says nothing");
        assert!(!hub_offers_machine(&json!({ "machine": {} })) && !hub_offers_machine(&json!({ "machine": { "v": 0 } })) && !hub_offers_machine(&json!({ "machine": "yes" })));
        assert!(hub_offers_machine(&json!({ "machine": { "v": 1 } })));
        assert_eq!(role_with(true, &json!({ "id": "h1" })), Role::AiHost, "newer app, older hub");
        assert_eq!(role_with(true, &json!({ "machine": { "v": 1 } })), Role::Machine);
        assert_eq!(role_with(false, &json!({ "machine": { "v": 1 } })), Role::AiHost, "older app, newer hub");
    }
    #[test]
    fn an_older_app_keeps_its_pc_engine_and_gets_no_machine_job() {
        let old: Vec<String> = ["relay", "update", "update-bin", "control", "signed-pc"].iter().map(|s| s.to_string()).collect();
        let older: Vec<String> = vec![];
        let new: Vec<String> = old.iter().cloned().chain(["machine".to_string()]).collect();
        assert!(!hub_may_send_machine_job(&old) && !hub_may_send_machine_job(&older) && hub_may_send_machine_job(&new));
        assert_eq!(pc_mode(&old, true), PcMode::OneFolderOlderApp);
        assert_eq!(pc_mode(&older, true), PcMode::OneFolderOlderApp, "no caps at all (hello of an old host)");
        assert_eq!(pc_mode(&new, true), PcMode::Machine);
        assert_eq!(pc_mode(&old, false), PcMode::None);
        assert_eq!(PcMode::OneFolderOlderApp.label(), "one folder (older app)");
        assert!(CAP_MACHINE.len() <= 20 && CAP_MACHINE.bytes().all(|x| x.is_ascii_lowercase() || x == b'-')); // what the hub's hello accepts
    }
    #[test]
    fn a_scope_with_a_higher_v_is_refused_whole_at_the_polls_boundary() {
        let ok = scope_from_poll(&json!({ "v": 1, "preset": "folder", "paths": [{ "path": "/srv/x", "access": "rw" }] }), 1_700_000_000).unwrap();
        assert!(ok.starts_with("one folder: /srv/x"), "{ok}");
        let newer = json!({ "v": 2, "preset": "folder", "paths": [{ "path": "/srv/x", "access": "rw" }], "futureThing": true });
        let e = scope_from_poll(&newer, 1_700_000_000).unwrap_err();
        assert!(e.contains("needs a newer hub") && e.contains("v2"), "{e}");
        assert!(scope_from_poll(&Value::String(newer.to_string()), 0).is_err(), "as text too");
        assert!(scope_from_poll(&json!("not json"), 0).is_err() && scope_from_poll(&Value::Null, 0).is_err() && scope_from_poll(&json!([1]), 0).is_err());
        // unknown keys at the known version are ignored (the library's rule), not refused
        assert!(scope_from_poll(&json!({ "v": 1, "futureKey": [1, 2] }), 0).is_ok());
    }
}
