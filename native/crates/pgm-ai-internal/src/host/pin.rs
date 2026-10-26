//! The host's pin of the hub's signing key (T-308, plans/linked-machines.md 4.4): the first key a hub announces in its hello
//! answer is kept in a file in the host folder; once one is known, every PC job must carry a request the hub signed for this
//! host's owner (machine::sign). Optional both ways: an older hub announces no key and signs nothing, and then jobs are
//! accepted as they always were (this includes the frozen JavaScript hub, the last fallback).
use pgm_net_internal::machine::sign::{kid_of_hex, Actor, Verifier, VerifyError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The file the first announced key is kept in.
pub const PIN_FILE: &str = "hub-signing-key.txt";

#[derive(Default)]
pub struct HubPin {
    slot: Mutex<Option<Arc<Verifier>>>,
}

fn short(k: &str) -> String { k.chars().take(16).collect() }

impl HubPin {
    pub fn new() -> HubPin { HubPin::default() }
    /// The verifier of the pinned key (T-313: handed to the PC role's `Machine`, so its door checks the hub's signature itself).
    /// Shared, so the replay cache is one.
    pub fn verifier(&self) -> Option<Arc<Verifier>> { self.slot.lock().unwrap_or_else(|e| e.into_inner()).clone() }
    /// Is a key pinned for the hub that is connected now?
    pub fn active(&self) -> bool { self.slot.lock().unwrap_or_else(|e| e.into_inner()).is_some() }
    /// What the hello answer said: `announced` = the hub's public key (hex, "" from an older hub), `owner` = the account this
    /// host is registered to. The first key is written to `dir`/[`PIN_FILE`] (owner-only). A DIFFERENT key later is not taken:
    /// the pin stays, so a request signed by the new key is refused, and the message (returned for the log) says how to
    /// accept it on purpose. Returns log lines.
    pub fn hello(&self, dir: &Path, announced: &str, owner: &str) -> Vec<String> {
        let mut log = vec![];
        let announced = announced.trim().to_lowercase();
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        // an older hub (or the frozen JavaScript hub) announces no key and signs nothing: nothing is verified while it is the
        // hub; the pin stays on file for when a hub that signs is back
        if announced.is_empty() { *slot = None; return log; }
        let file: PathBuf = dir.join(PIN_FILE);
        let pinned = std::fs::read_to_string(&file).unwrap_or_default().trim().to_lowercase();
        let key = if pinned.is_empty() || kid_of_hex(&pinned).is_empty() {
            if std::fs::write(&file, format!("{announced}\n")).is_ok() { pgm_core_internal::fs::private(&file); log.push(format!("pinned the hub's signing key {}: PC requests must now be signed by it", short(&announced))); }
            announced.clone()
        } else {
            if pinned != announced { log.push(format!("the hub announced a different signing key ({}) than the one this host pinned ({}); keeping the pinned one, so requests signed by the new key are refused. If the hub was moved on purpose, delete {} and restart this host", short(&announced), short(&pinned), file.display())); }
            pinned
        };
        if slot.as_ref().map(|v| v.owner() == owner && v.kid() == kid_of_hex(&key)).unwrap_or(false) { return log; } // same key and owner: keep it, and its replay cache
        match Verifier::new(&key, owner, "") { Ok(v) => *slot = Some(Arc::new(v)), Err(e) => { *slot = None; log.push(format!("the hub's signing key is unusable ({e})")); } }
        log
    }
    /// Checks one PC job: Ok(None) = nothing is pinned (accepted as before), Ok(Some(actor)) = signed by the hub for this host's
    /// owner, Err = refused (the reason is for the log and the answer).
    pub fn check(&self, header: &str, tool: &str, body: &[u8], now: i64) -> Result<Option<Actor>, VerifyError> {
        let Some(v) = self.slot.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return Ok(None) };
        v.verify(header, tool, body, now).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgm_net_internal::machine::sign::{Envelope, HubKey};

    const NOW: i64 = 1_790_000_000;
    fn dir(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("vr-pin-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn actor(a: &str) -> Actor { Actor { account: a.into(), session: String::new(), run: "r1".into() } }
    fn signed(k: &HubKey, owner: &str, acct: &str, tool: &str, body: &[u8]) -> String { k.sign(&Envelope::new(owner, actor(acct), tool, body, "h1", NOW)).to_header() }

    #[test]
    fn no_key_announced_means_unsigned_jobs_work_as_before() {
        let d = dir("none");
        let p = HubPin::new();
        assert!(p.hello(&d, "", "sponge").is_empty());
        assert!(!p.active());
        assert_eq!(p.check("", "list", b"{}", NOW), Ok(None));
        assert!(!d.join(PIN_FILE).exists());
    }

    #[test]
    fn a_pinned_key_demands_a_signature_for_the_hosts_owner() {
        let d = dir("pinned");
        let (hub, liar) = (HubKey::generate(), HubKey::generate());
        let p = HubPin::new();
        let log = p.hello(&d, &hub.public_hex(), "sponge");
        assert!(log.iter().any(|l| l.contains("pinned")));
        assert!(p.active());
        assert_eq!(std::fs::read_to_string(d.join(PIN_FILE)).unwrap().trim(), hub.public_hex());
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(d.join(PIN_FILE)).unwrap().permissions().mode() & 0o777, 0o600); }
        // signed by the hub for the owner: passes (once)
        let ok = signed(&hub, "sponge", "sponge", "write", b"{\"path\":\"a\"}");
        assert_eq!(p.check(&ok, "write", b"{\"path\":\"a\"}", NOW).unwrap().unwrap().account, "sponge");
        assert_eq!(p.check(&ok, "write", b"{\"path\":\"a\"}", NOW), Err(VerifyError::Replay));
        // no signature at all (a hub stub that strips it), another key, another owner, another actor: refused
        assert!(matches!(p.check("", "write", b"{}", NOW), Err(VerifyError::Format(_))));
        assert_eq!(p.check(&signed(&liar, "sponge", "sponge", "write", b"{}"), "write", b"{}", NOW), Err(VerifyError::UnknownKey));
        assert_eq!(p.check(&signed(&hub, "gee", "gee", "write", b"{}"), "write", b"{}", NOW), Err(VerifyError::WrongOwner));
        assert_eq!(p.check(&signed(&hub, "sponge", "gee", "write", b"{}"), "write", b"{}", NOW), Err(VerifyError::ActorNotOwner));
        assert_eq!(p.check(&signed(&hub, "sponge", "admin", "delete", b"{}"), "delete", b"{}", NOW), Err(VerifyError::ActorNotOwner));
    }

    #[test]
    fn a_different_key_later_is_not_taken_and_a_hello_again_keeps_the_replay_cache() {
        let d = dir("rekey");
        let (hub, other) = (HubKey::generate(), HubKey::generate());
        let p = HubPin::new();
        p.hello(&d, &hub.public_hex(), "sponge");
        let ok = signed(&hub, "sponge", "sponge", "list", b"{}");
        assert!(p.check(&ok, "list", b"{}", NOW).is_ok());
        // the same key again (a re-hello): still pinned, the used nonce is still remembered
        assert!(p.hello(&d, &hub.public_hex(), "sponge").is_empty());
        assert_eq!(p.check(&ok, "list", b"{}", NOW), Err(VerifyError::Replay));
        // another key announced: told in the log, the pin stays
        let log = p.hello(&d, &other.public_hex(), "sponge");
        assert!(log.iter().any(|l| l.contains("different signing key")));
        assert!(p.check(&signed(&other, "sponge", "sponge", "list", b"{}"), "list", b"{}", NOW).is_err());
        assert!(p.check(&signed(&hub, "sponge", "sponge", "list", b"{ }"), "list", b"{ }", NOW).is_ok());
        // a fresh process (a new HubPin) reads the same pin from the file
        let p2 = HubPin::new();
        p2.hello(&d, &other.public_hex(), "sponge");
        assert!(p2.check(&signed(&hub, "sponge", "sponge", "list", b"{  }"), "list", b"{  }", NOW).is_ok());
        // the older hub in between (no key): unsigned works while it is the hub, the pin is back with the next signing hub
        p.hello(&d, "", "sponge");
        assert_eq!(p.check("", "list", b"{}", NOW), Ok(None));
        p.hello(&d, &hub.public_hex(), "sponge");
        assert!(p.check("", "list", b"{}", NOW).is_err());
    }
}
