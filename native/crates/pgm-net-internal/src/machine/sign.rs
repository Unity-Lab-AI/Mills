//! Signed machine requests (plans/linked-machines.md section 4.2 and 4.4, T-308).
//!
//! The hub holds ONE ed25519 signing key ([`HubKey`], made once, kept in the hub's data dir, owner-only file). Every PC /
//! machine tool request it forwards to an AI host carries an [`Envelope`] (owner, actor, tool, a digest of the arguments,
//! a nonce, issuedAt, expiresAt) signed with it. The machine side, a [`Verifier`], pins the hub's public key and refuses:
//! a bad signature, a request signed by another key, an expired (or not yet valid) request, a repeated nonce (a bounded
//! replay cache), an owner that is not its own link, `actor.account != owner`, another machine id, a tool or arguments that
//! are not the ones signed.
//!
//! What is signed is the exact payload string (not a re-serialised value), so there is nothing to canonicalise: the wire
//! form is `{"payload": "<envelope json>", "sig": "<hex>", "kid": "<key id>"}`, put in one header value.
//! Pure logic with the clock passed in: no hub, no network.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier as _, VerifyingKey};
use pgm_core_internal::crypto::{random_hex, sha256_hex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// The header (a job header on the host link) that carries a [`SignedRequest`].
pub const HEADER: &str = "x-vr-machine-sig";
/// The cap a host announces in hello: it verifies signed PC requests once it knows the hub's key (optional; an older
/// host never announces it and keeps working unverified).
pub const CAP_SIGNED: &str = "signed-pc";
/// What a request lives (seconds) when the hub does not say otherwise.
pub const TTL_SECS: i64 = 60;
/// The longest life a verifier accepts (seconds), whatever the envelope says.
pub const MAX_TTL_SECS: i64 = 300;
/// Clock difference tolerated between hub and machine (seconds).
pub const SKEW_SECS: i64 = 30;
/// How many nonces a verifier keeps (a full cache of live nonces refuses new requests: fail closed).
pub const REPLAY_CAP: usize = 4096;
const DOMAIN: &[u8] = b"vr-machine-request-v1\n";

/// On whose behalf a request is made: taken by the hub from its own records, never from a field the caller typed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub account: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub run: String,
}

/// What is signed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub owner: String,
    pub actor: Actor,
    pub tool: String,
    #[serde(rename = "argsDigest")]
    pub args_digest: String,
    pub nonce: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: i64,
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,
    /// the machine's id when the request names one (empty: whichever machine receives it, its owner check still applies)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine: String,
}

impl Envelope {
    /// A fresh envelope: `args` are the exact request body bytes the machine will receive.
    pub fn new(owner: &str, actor: Actor, tool: &str, args: &[u8], machine: &str, now: i64) -> Envelope {
        Envelope { v: 1, owner: owner.into(), actor, tool: tool.into(), args_digest: digest(args), nonce: random_hex(16), issued_at: now, expires_at: now + TTL_SECS, machine: machine.into() }
    }
}

/// sha256 of the arguments, lowercase hex.
pub fn digest(args: &[u8]) -> String { sha256_hex(args) }

/// An envelope with its signature, as sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRequest {
    pub payload: String,
    pub sig: String,
    #[serde(default)]
    pub kid: String,
}
impl SignedRequest {
    /// One header value (compact JSON, no line breaks).
    pub fn to_header(&self) -> String { serde_json::to_string(self).unwrap_or_default() }
    pub fn from_header(h: &str) -> Result<SignedRequest, VerifyError> {
        if h.is_empty() || h.len() > 16 * 1024 { return Err(VerifyError::Format("missing or too large".into())); }
        serde_json::from_str(h).map_err(|e| VerifyError::Format(e.to_string()))
    }
}

/// Why a request was refused. `Display` is the message a person may read in a log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    Format(String),
    UnknownKey,
    BadSignature,
    Version,
    Expired,
    NotYetValid,
    TooLong,
    WrongTool,
    WrongArgs,
    WrongOwner,
    ActorNotOwner,
    WrongMachine,
    Replay,
    CacheFull,
}
impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::Format(m) => write!(f, "the request signature is malformed ({m})"),
            VerifyError::UnknownKey => write!(f, "the request is signed with a key this machine does not know"),
            VerifyError::BadSignature => write!(f, "the request signature is not valid"),
            VerifyError::Version => write!(f, "the request is a version this machine does not know"),
            VerifyError::Expired => write!(f, "the request has expired"),
            VerifyError::NotYetValid => write!(f, "the request is dated in the future (check the clocks)"),
            VerifyError::TooLong => write!(f, "the request lives longer than a machine accepts"),
            VerifyError::WrongTool => write!(f, "the request was signed for another tool"),
            VerifyError::WrongArgs => write!(f, "the arguments are not the ones that were signed"),
            VerifyError::WrongOwner => write!(f, "the request is for a machine owned by someone else"),
            VerifyError::ActorNotOwner => write!(f, "the request was made by someone who is not this machine's owner"),
            VerifyError::WrongMachine => write!(f, "the request names another machine"),
            VerifyError::Replay => write!(f, "this request was already used (repeated nonce)"),
            VerifyError::CacheFull => write!(f, "too many live requests; try again in a minute"),
        }
    }
}
impl std::error::Error for VerifyError {}

fn kid_of(public: &[u8; 32]) -> String { sha256_hex(public)[..16].to_string() }
/// The id of a public key given as hex ("" when it is not a key).
pub fn kid_of_hex(public_hex: &str) -> String { hex::decode(public_hex.trim()).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()).map(|b| kid_of(&b)).unwrap_or_default() }

/// The hub's signing key: one per hub.
pub struct HubKey {
    key: SigningKey,
}
impl HubKey {
    /// A key from 32 bytes (tests, or a stored seed).
    pub fn from_seed(seed: [u8; 32]) -> HubKey { HubKey { key: SigningKey::from_bytes(&seed) } }
    /// A new random key.
    pub fn generate() -> HubKey {
        let seed: [u8; 32] = hex::decode(random_hex(32)).ok().and_then(|b| b.try_into().ok()).expect("32 random bytes");
        HubKey::from_seed(seed)
    }
    /// The key kept in `path` (hex seed, one line, owner-only file); made there the first time. A file that exists but is
    /// not a key is an error: it is never silently replaced, since every machine that pinned the old key would refuse
    /// the hub from then on.
    pub fn load_or_create(path: &Path) -> std::io::Result<HubKey> {
        match std::fs::read_to_string(path) {
            Ok(t) => {
                let seed = hex::decode(t.trim()).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is not a hub signing key (expected 64 hex characters); move it away to make a new one", path.display())))?;
                pgm_core_internal::fs::private(path);
                Ok(HubKey::from_seed(seed))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
                let k = HubKey::generate();
                let tmp = path.with_extension("key.tmp");
                {
                    use std::io::Write;
                    let mut o = std::fs::OpenOptions::new();
                    o.write(true).create(true).truncate(true);
                    #[cfg(unix)]
                    { use std::os::unix::fs::OpenOptionsExt; o.mode(0o600); } // owner-only from the first byte
                    let mut f = o.open(&tmp)?;
                    f.write_all(format!("{}\n", hex::encode(k.key.to_bytes())).as_bytes())?;
                    f.sync_all()?;
                }
                pgm_core_internal::fs::private(&tmp);
                match std::fs::hard_link(&tmp, path) { // never replace a key another thread or process just made
                    Ok(()) => { let _ = std::fs::remove_file(&tmp); Ok(k) }
                    Err(e2) if e2.kind() == std::io::ErrorKind::AlreadyExists => { let _ = std::fs::remove_file(&tmp); HubKey::load_or_create(path) }
                    Err(_) => { std::fs::rename(&tmp, path)?; Ok(k) }
                }
            }
            Err(e) => Err(e),
        }
    }
    /// The public key, lowercase hex (what a machine pins).
    pub fn public_hex(&self) -> String { hex::encode(self.key.verifying_key().to_bytes()) }
    pub fn kid(&self) -> String { kid_of(&self.key.verifying_key().to_bytes()) }
    pub fn sign(&self, env: &Envelope) -> SignedRequest {
        let payload = serde_json::to_string(env).unwrap_or_default();
        let sig = self.key.sign(&[DOMAIN, payload.as_bytes()].concat());
        SignedRequest { payload, sig: hex::encode(sig.to_bytes()), kid: self.kid() }
    }
}

/// The machine's side: pins the hub's public key and knows its own link.
pub struct Verifier {
    hub: VerifyingKey,
    kid: String,
    owner: String,
    machine: String,
    seen: Mutex<HashMap<String, i64>>,
}
impl Verifier {
    /// `hub_public_hex`: the hub's key as announced at link / hello; `owner`: the account this machine is linked to (its own
    /// record, never learned from a request); `machine`: its own id ("" when it has none, then an envelope naming one is
    /// not checked against an id).
    pub fn new(hub_public_hex: &str, owner: &str, machine: &str) -> Result<Verifier, VerifyError> {
        let b: [u8; 32] = hex::decode(hub_public_hex.trim()).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| VerifyError::Format("the hub key is not 64 hex characters".into()))?;
        let hub = VerifyingKey::from_bytes(&b).map_err(|e| VerifyError::Format(e.to_string()))?;
        Ok(Verifier { hub, kid: kid_of(&b), owner: owner.into(), machine: machine.into(), seen: Mutex::new(HashMap::new()) })
    }
    pub fn owner(&self) -> &str { &self.owner }
    pub fn kid(&self) -> &str { &self.kid }
    /// Checks one request: the header value, the tool name as the machine received it, the exact body bytes, the clock.
    /// Returns the actor on success (for the machine's own audit line). A request that passes uses its nonce up.
    pub fn verify(&self, header: &str, tool: &str, body: &[u8], now: i64) -> Result<Actor, VerifyError> {
        let sr = SignedRequest::from_header(header)?;
        if !sr.kid.is_empty() && sr.kid != self.kid { return Err(VerifyError::UnknownKey); }
        let sig: [u8; 64] = hex::decode(&sr.sig).ok().and_then(|b| b.try_into().ok()).ok_or(VerifyError::BadSignature)?;
        self.hub.verify(&[DOMAIN, sr.payload.as_bytes()].concat(), &Signature::from_bytes(&sig)).map_err(|_| VerifyError::BadSignature)?;
        let env: Envelope = serde_json::from_str(&sr.payload).map_err(|e| VerifyError::Format(e.to_string()))?;
        if env.v != 1 { return Err(VerifyError::Version); }
        if env.expires_at.saturating_add(SKEW_SECS) < now { return Err(VerifyError::Expired); }
        if env.issued_at > now.saturating_add(SKEW_SECS) { return Err(VerifyError::NotYetValid); }
        if env.expires_at.saturating_sub(env.issued_at) > MAX_TTL_SECS || env.expires_at < env.issued_at { return Err(VerifyError::TooLong); }
        if env.tool != tool.trim_start_matches('/') { return Err(VerifyError::WrongTool); }
        if env.args_digest != digest(body) { return Err(VerifyError::WrongArgs); }
        if self.owner.is_empty() || env.owner != self.owner { return Err(VerifyError::WrongOwner); }
        if env.actor.account != env.owner { return Err(VerifyError::ActorNotOwner); }
        if !self.machine.is_empty() && !env.machine.is_empty() && env.machine != self.machine { return Err(VerifyError::WrongMachine); }
        if env.nonce.len() < 16 || env.nonce.len() > 128 { return Err(VerifyError::Format("nonce".into())); }
        // last, so only a request that is otherwise good uses a nonce up
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, exp| exp.saturating_add(SKEW_SECS) >= now);
        if seen.contains_key(&env.nonce) { return Err(VerifyError::Replay); }
        if seen.len() >= REPLAY_CAP { return Err(VerifyError::CacheFull); }
        seen.insert(env.nonce.clone(), env.expires_at);
        Ok(env.actor)
    }
    /// How many nonces are held (a test).
    pub fn held(&self) -> usize { self.seen.lock().unwrap_or_else(|e| e.into_inner()).len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000;
    fn actor(a: &str) -> Actor { Actor { account: a.into(), session: "s2".into(), run: "r41".into() } }
    fn setup() -> (HubKey, Verifier) {
        let k = HubKey::generate();
        let v = Verifier::new(&k.public_hex(), "sponge", "m1").unwrap();
        (k, v)
    }
    fn good(k: &HubKey, tool: &str, body: &[u8]) -> String { k.sign(&Envelope::new("sponge", actor("sponge"), tool, body, "m1", NOW)).to_header() }

    #[test]
    fn a_good_request_passes_and_names_the_actor() {
        let (k, v) = setup();
        let a = v.verify(&good(&k, "write", b"{\"path\":\"a\"}"), "write", b"{\"path\":\"a\"}", NOW + 5).unwrap();
        assert_eq!(a, actor("sponge"));
        assert_eq!(v.owner(), "sponge");
        // the tool may come as the job path, with a slash
        assert!(v.verify(&good(&k, "list", b"{}"), "/list", b"{}", NOW).is_ok());
    }

    #[test]
    fn bad_signature_other_key_and_tampering_are_refused() {
        let (k, v) = setup();
        let h = good(&k, "write", b"{}");
        // a flipped signature bit
        let mut sr = SignedRequest::from_header(&h).unwrap();
        let mut sig = hex::decode(&sr.sig).unwrap(); sig[0] ^= 1; sr.sig = hex::encode(sig);
        assert_eq!(v.verify(&sr.to_header(), "write", b"{}", NOW), Err(VerifyError::BadSignature));
        // an edited payload (another owner) under the old signature
        let mut sr = SignedRequest::from_header(&h).unwrap();
        sr.payload = sr.payload.replace("sponge", "gee");
        assert_eq!(v.verify(&sr.to_header(), "write", b"{}", NOW), Err(VerifyError::BadSignature));
        // garbage
        for junk in ["", "nope", "{}", "{\"payload\":1}", &"x".repeat(20000)] { assert!(matches!(v.verify(junk, "write", b"{}", NOW), Err(VerifyError::Format(_))), "{junk:.20}"); }
        // a signature that is not hex
        let mut sr = SignedRequest::from_header(&h).unwrap(); sr.sig = "zz".into();
        assert_eq!(v.verify(&sr.to_header(), "write", b"{}", NOW), Err(VerifyError::BadSignature));
        assert_eq!(v.held(), 0, "nothing refused used a nonce up");
    }

    #[test]
    fn a_lying_hub_stub_is_refused() {
        let (_real, v) = setup();
        let liar = HubKey::generate();
        // signed by a different key, for the right owner: refused (same kid claimed, or its own kid)
        let h = liar.sign(&Envelope::new("sponge", actor("sponge"), "write", b"{}", "m1", NOW)).to_header();
        assert_eq!(v.verify(&h, "write", b"{}", NOW), Err(VerifyError::UnknownKey));
        let mut sr = SignedRequest::from_header(&h).unwrap(); sr.kid = v.kid().to_string(); // claiming the pinned key's id
        assert_eq!(v.verify(&sr.to_header(), "write", b"{}", NOW), Err(VerifyError::BadSignature));
        sr.kid = String::new(); // claiming nothing
        assert_eq!(v.verify(&sr.to_header(), "write", b"{}", NOW), Err(VerifyError::BadSignature));
    }

    #[test]
    fn a_request_for_another_owner_or_by_another_actor_is_refused() {
        let (k, v) = setup();
        // a request for gee's machine, signed by the real hub key: this machine is sponge's
        let for_gee = k.sign(&Envelope::new("gee", actor("gee"), "write", b"{}", "m1", NOW)).to_header();
        assert_eq!(v.verify(&for_gee, "write", b"{}", NOW), Err(VerifyError::WrongOwner));
        // sponge's machine, but gee's run (or an admin's): actor.account != owner
        let gee_run = k.sign(&Envelope::new("sponge", actor("gee"), "write", b"{}", "m1", NOW)).to_header();
        assert_eq!(v.verify(&gee_run, "write", b"{}", NOW), Err(VerifyError::ActorNotOwner));
        let admin_run = k.sign(&Envelope::new("sponge", actor("admin"), "delete", b"{}", "m1", NOW)).to_header();
        assert_eq!(v.verify(&admin_run, "delete", b"{}", NOW), Err(VerifyError::ActorNotOwner));
        // another machine id
        let m2 = k.sign(&Envelope::new("sponge", actor("sponge"), "write", b"{}", "m2", NOW)).to_header();
        assert_eq!(v.verify(&m2, "write", b"{}", NOW), Err(VerifyError::WrongMachine));
        // a verifier with no owner (a machine not linked to anyone) accepts nothing
        let orphan = Verifier::new(&k.public_hex(), "", "").unwrap();
        assert_eq!(orphan.verify(&good(&k, "write", b"{}"), "write", b"{}", NOW), Err(VerifyError::WrongOwner));
        // no machine id named: the owner rules still apply
        let unnamed = k.sign(&Envelope::new("sponge", actor("sponge"), "write", b"{}", "", NOW)).to_header();
        assert!(v.verify(&unnamed, "write", b"{}", NOW).is_ok());
    }

    #[test]
    fn expired_future_and_too_long_requests_are_refused() {
        let (k, v) = setup();
        let h = good(&k, "write", b"{}");
        assert!(v.verify(&h, "write", b"{}", NOW + TTL_SECS + SKEW_SECS).is_ok(), "within the tolerated skew");
        let h2 = good(&k, "write", b"{ }");
        assert_eq!(v.verify(&h2, "write", b"{ }", NOW + TTL_SECS + SKEW_SECS + 1), Err(VerifyError::Expired));
        let h3 = k.sign(&Envelope::new("sponge", actor("sponge"), "write", b"{}", "m1", NOW + 3600)).to_header();
        assert_eq!(v.verify(&h3, "write", b"{}", NOW), Err(VerifyError::NotYetValid));
        let mut long = Envelope::new("sponge", actor("sponge"), "write", b"{}", "m1", NOW); long.expires_at = NOW + MAX_TTL_SECS + 1;
        assert_eq!(v.verify(&k.sign(&long).to_header(), "write", b"{}", NOW), Err(VerifyError::TooLong));
        let mut back = Envelope::new("sponge", actor("sponge"), "write", b"{}", "m1", NOW); back.expires_at = NOW - 10; back.issued_at = NOW;
        assert!(v.verify(&k.sign(&back).to_header(), "write", b"{}", NOW).is_err());
    }

    #[test]
    fn a_repeated_nonce_is_refused_and_the_cache_is_bounded() {
        let (k, v) = setup();
        let h = good(&k, "write", b"{}");
        assert!(v.verify(&h, "write", b"{}", NOW).is_ok());
        assert_eq!(v.verify(&h, "write", b"{}", NOW + 1), Err(VerifyError::Replay));
        // once the request and the skew have passed it is expired anyway, and the cache lets the nonce go
        assert_eq!(v.verify(&h, "write", b"{}", NOW + TTL_SECS + SKEW_SECS + 1), Err(VerifyError::Expired));
        assert_eq!(v.held(), 1);
        let (_, v2) = setup();
        let _ = v2; // an independent verifier has its own cache
        // fill: REPLAY_CAP live nonces, then a new one is refused (fail closed), an old one is still a replay
        let v3 = Verifier::new(&k.public_hex(), "sponge", "m1").unwrap();
        let first = good(&k, "write", b"{}");
        assert!(v3.verify(&first, "write", b"{}", NOW).is_ok());
        for _ in 1..REPLAY_CAP { assert!(v3.verify(&good(&k, "write", b"{}"), "write", b"{}", NOW).is_ok()); }
        assert_eq!(v3.held(), REPLAY_CAP);
        assert_eq!(v3.verify(&good(&k, "write", b"{}"), "write", b"{}", NOW), Err(VerifyError::CacheFull));
        assert_eq!(v3.verify(&first, "write", b"{}", NOW), Err(VerifyError::Replay));
        // later the old nonces are pruned and it works again
        assert!(v3.verify(&k.sign(&Envelope::new("sponge", actor("sponge"), "write", b"{}", "m1", NOW + 200)).to_header(), "write", b"{}", NOW + 200).is_ok());
        assert_eq!(v3.held(), 1);
    }

    #[test]
    fn the_tool_and_the_arguments_are_bound() {
        let (k, v) = setup();
        let h = good(&k, "read", b"{\"path\":\"a.txt\"}");
        assert_eq!(v.verify(&h, "write", b"{\"path\":\"a.txt\"}", NOW), Err(VerifyError::WrongTool));
        assert_eq!(v.verify(&h, "read", b"{\"path\":\"b.txt\"}", NOW), Err(VerifyError::WrongArgs));
        assert_eq!(v.verify(&h, "read", b"", NOW), Err(VerifyError::WrongArgs));
        assert!(v.verify(&h, "read", b"{\"path\":\"a.txt\"}", NOW).is_ok());
    }

    #[test]
    fn the_hub_key_is_made_once_and_kept_owner_only() {
        let d = std::env::temp_dir().join(format!("pgm-sign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("data").join("hub-signing.key");
        let a = HubKey::load_or_create(&f).unwrap();
        let b = HubKey::load_or_create(&f).unwrap();
        assert_eq!(a.public_hex(), b.public_hex(), "the same key every time");
        assert_eq!(a.public_hex().len(), 64);
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600); }
        // a loose file is tightened on load
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap(); let _ = HubKey::load_or_create(&f).unwrap(); assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600); }
        // a file that is not a key is an error, never replaced
        std::fs::write(&f, "not a key").unwrap();
        assert!(HubKey::load_or_create(&f).is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "not a key");
        // no temp file is left behind
        assert!(!d.join("data").join("hub-signing.key.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
        // and the key signs what a verifier pinned to it accepts
        let v = Verifier::new(&a.public_hex(), "sponge", "").unwrap();
        assert!(v.verify(&a.sign(&Envelope::new("sponge", actor("sponge"), "list", b"{}", "", NOW)).to_header(), "list", b"{}", NOW).is_ok());
    }

    #[test]
    fn a_bad_pinned_key_is_refused_at_construction() {
        assert!(Verifier::new("", "sponge", "").is_err());
        assert!(Verifier::new("abcd", "sponge", "").is_err());
        assert!(Verifier::new(&"zz".repeat(32), "sponge", "").is_err());
    }
}
