//! Accounts & Authentication (board T-131), moved from bridge/server.js; since 5.0.0 the work itself is
//! pgm-identity-internal's (PGM_Identity_Internal): sign-in (scrypt, device locks with several devices, per-address
//! limits), the sessions (12 hours, kept as sha256 of the token in bridge/front-sessions.json), the accounts. What is the
//! hub's is here: which config says the default account and whether nginx is in front, the "moved" answer while the hub
//! moves house, and the hub's own answer type.
//! What stays in the JavaScript hub: the accounts' profiles and settings (it reloads them when one is made or deleted)
//! and what a sign-in starts there (Unity's greeting), which it is told through /internal/front/signed-in.
use serde_json::Value;
use std::path::Path;

use crate::front::{json as jresp, Req};
use crate::modules::Routed;
#[allow(unused_imports)]
pub use pgm_core_internal::time::{iso_at, iso_now};
#[allow(unused_imports)]
pub use pgm_identity_internal::{admin_in, hash_password, lock_devices, new_password, token_key, valid_username, Account};

/// The hub's accounts (pgm-identity-internal's Auth: everything it does, plus the hub's own sign-in answer).
pub struct Auth(pgm_identity_internal::Auth);
impl std::ops::Deref for Auth { type Target = pgm_identity_internal::Auth; fn deref(&self) -> &Self::Target { &self.0 } }
impl Auth {
    pub fn new(root: &Path, cfg: &crate::modules::Config) -> Auth {
        let default_user = cfg.get("defaultUsername").and_then(Value::as_str).unwrap_or("mills").to_string();
        let behind_proxy = cfg.get("behindProxy").and_then(Value::as_bool) == Some(true);
        Auth(pgm_identity_internal::Auth::new(root, &default_user, behind_proxy, "[vr-hub]"))
    }
    /// POST /api/login. `maintenance`: where the hub has moved to (then nobody signs in here).
    pub async fn login(&self, r: &Req, maintenance: Option<String>) -> Routed {
        let refuse = maintenance.map(|w| format!("VibeRoller has moved to {w}: open it there and sign in as usual. This copy is read-only now."));
        match self.0.login(r, refuse).await {
            pgm_identity_internal::Login::Answer(status, v) => Routed::Answer(jresp(status, &v)),
            pgm_identity_internal::Login::SignedIn { token, user, client, ip } => Routed::SignedIn { token, user, client, ip },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_and_passwords_through_the_libraries() {
        assert!(iso_now().ends_with('Z') && iso_now().len() == 24);
        let h = hash_password("hello there");
        assert!(pgm_identity_internal::check_password(&h, "hello there") && !pgm_identity_internal::check_password(&h, "hello therE"));
        assert_eq!(new_password().len(), 20);
    }
}
