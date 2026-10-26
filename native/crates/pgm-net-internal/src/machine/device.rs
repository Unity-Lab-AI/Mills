//! The per-device key of a linked machine (plans/linked-machines.md 3.2, T-309): an ed25519 key pair made on the
//! machine, its private half never leaves the machine's state dir (owner-only file), its public half is given to the
//! hub at link time. The machine signs each reply with it; the hub checks the signature against the key it recorded
//! at approval. A token copied off a disk is then not enough to answer for the machine.
//!
//! What is signed (one fixed text, so nobody can move a signature to another request, machine or body):
//! `vr-machine-reply/1 \n <machine id> \n <request id> \n <sha256 hex of the body bytes>`.
//! Also here: the machine's own token file and what it does on a 401 "revoked" (wipe the token and the key).
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use pgm_core_internal::crypto::{random_bytes, sha256_hex};
use std::io::Write;
use std::path::{Path, PathBuf};

const KEY_FILE: &str = "device-key";
const TOKEN_FILE: &str = "token";
const CONTEXT: &str = "vr-machine-reply/1";

/// The text that is signed for one reply.
pub fn reply_message(machine_id: &str, request_id: &str, body: &[u8]) -> Vec<u8> {
    format!("{CONTEXT}\n{machine_id}\n{request_id}\n{}", sha256_hex(body)).into_bytes()
}

/// A public key as the hub gets it: 64 hex characters (32 bytes). Anything else is refused.
pub fn parse_public(s: &str) -> Result<VerifyingKey, String> {
    let b = hex::decode(s.trim()).map_err(|_| "The machine's public key must be 64 hex characters.".to_string())?;
    let a: [u8; 32] = b.try_into().map_err(|_| "The machine's public key must be 32 bytes (ed25519).".to_string())?;
    VerifyingKey::from_bytes(&a).map_err(|_| "That is not a valid ed25519 public key.".to_string())
}

/// The hub's check of a reply: `sig` is 128 hex characters. False on any fault (bad key, bad hex, wrong message).
pub fn verify_reply(public_hex: &str, machine_id: &str, request_id: &str, body: &[u8], sig_hex: &str) -> bool {
    let Ok(pk) = parse_public(public_hex) else { return false };
    let Ok(sb) = hex::decode(sig_hex.trim()) else { return false };
    let Ok(sa) = <[u8; 64]>::try_from(sb.as_slice()) else { return false };
    pk.verify(&reply_message(machine_id, request_id, body), &Signature::from_bytes(&sa)).is_ok()
}

/// A short fingerprint of a public key for the approval screen (the first 16 hex of its sha256, in groups of four).
pub fn fingerprint(public_hex: &str) -> String {
    let h = sha256_hex(public_hex.trim().as_bytes());
    h[..16].as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
}

/// The machine's device key.
pub struct DeviceKey(SigningKey);

impl DeviceKey {
    pub fn generate() -> DeviceKey {
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&random_bytes(32));
        DeviceKey(SigningKey::from_bytes(&seed))
    }
    pub fn from_seed_hex(h: &str) -> Option<DeviceKey> {
        let b: [u8; 32] = hex::decode(h.trim()).ok()?.try_into().ok()?;
        Some(DeviceKey(SigningKey::from_bytes(&b)))
    }
    pub fn public_hex(&self) -> String { hex::encode(self.0.verifying_key().to_bytes()) }
    /// Signs a reply (hex).
    pub fn sign_reply(&self, machine_id: &str, request_id: &str, body: &[u8]) -> String {
        hex::encode(self.0.sign(&reply_message(machine_id, request_id, body)).to_bytes())
    }
}

/// Writes a file that only its owner can read, owner-only from its first byte on Linux.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        { use std::os::unix::fs::OpenOptionsExt; o.mode(0o600); }
        let mut f = o.open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    pgm_core_internal::fs::private(&tmp);
    std::fs::rename(&tmp, path)
}

/// The machine's state dir parts that belong to the link: the device key and the token (owner-only files in an
/// owner-only dir; Linux `~/.local/state/vibe-roller/machine/`, Windows `%LOCALAPPDATA%\VibeRoller\machine\`).
pub struct LinkStore {
    dir: PathBuf,
}

impl LinkStore {
    pub fn new(dir: &Path) -> std::io::Result<LinkStore> {
        pgm_core_internal::fs::private_dir(dir)?;
        Ok(LinkStore { dir: dir.to_path_buf() })
    }
    pub fn key_file(&self) -> PathBuf { self.dir.join(KEY_FILE) }
    pub fn token_file(&self) -> PathBuf { self.dir.join(TOKEN_FILE) }
    /// The device key: read, or made and saved the first time (a damaged file is replaced: the machine then links again).
    pub fn load_or_create_key(&self) -> std::io::Result<DeviceKey> {
        if let Some(k) = std::fs::read_to_string(self.key_file()).ok().and_then(|t| DeviceKey::from_seed_hex(&t)) { return Ok(k); }
        let k = DeviceKey::generate();
        write_private(&self.key_file(), &hex::encode(k.0.to_bytes()))?;
        Ok(k)
    }
    pub fn save_token(&self, token: &str) -> std::io::Result<()> { write_private(&self.token_file(), token) }
    pub fn token(&self) -> Option<String> { std::fs::read_to_string(self.token_file()).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) }
    pub fn linked(&self) -> bool { self.token().is_some() }
    /// Forget the link: the token and the key go (a new link makes a new key).
    pub fn wipe(&self) {
        let _ = std::fs::remove_file(self.token_file());
        let _ = std::fs::remove_file(self.key_file());
    }
    /// What the machine does with an answer from the hub: a 401 that says "revoked" wipes the link and returns true.
    /// (Any other 401 or an error is not a revoke: the machine keeps its link and retries.)
    pub fn handle_answer(&self, status: u16, body: &serde_json::Value) -> bool {
        if is_revoked(status, body) { self.wipe(); true } else { false }
    }
}

/// Is this answer the hub's "this machine was unlinked"?
pub fn is_revoked(status: u16, body: &serde_json::Value) -> bool {
    status == 401 && (body["revoked"].as_bool() == Some(true) || body["error"].as_str() == Some("revoked"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dir() -> PathBuf { std::env::temp_dir().join(format!("vr-devkey-{}", pgm_core_internal::crypto::random_hex(4))) }

    #[test]
    fn sign_verify_and_what_is_bound() {
        let k = DeviceKey::generate();
        let (pk, body) = (k.public_hex(), b"{\"ok\":true}".as_slice());
        assert_eq!(pk.len(), 64);
        let sig = k.sign_reply("m1", "req1", body);
        assert!(verify_reply(&pk, "m1", "req1", body, &sig));
        assert!(!verify_reply(&pk, "m2", "req1", body, &sig), "another machine");
        assert!(!verify_reply(&pk, "m1", "req2", body, &sig), "another request");
        assert!(!verify_reply(&pk, "m1", "req1", b"{\"ok\":false}", &sig), "another body");
        assert!(!verify_reply(&DeviceKey::generate().public_hex(), "m1", "req1", body, &sig), "another key");
        assert!(!verify_reply(&pk, "m1", "req1", body, "zz") && !verify_reply("nope", "m1", "req1", body, &sig));
        assert!(parse_public("abcd").is_err() && parse_public(&pk).is_ok());
        assert_eq!(fingerprint(&pk).len(), 19);
    }

    #[test]
    fn store_is_owner_only_and_wipes() {
        let d = dir();
        let s = LinkStore::new(&d).unwrap();
        let k = s.load_or_create_key().unwrap();
        assert_eq!(s.load_or_create_key().unwrap().public_hex(), k.public_hex(), "the same key the second time");
        s.save_token("tok").unwrap();
        assert_eq!(s.token().as_deref(), Some("tok"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [s.key_file(), s.token_file()] { assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600); }
            assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert!(!s.handle_answer(401, &serde_json::json!({ "error": "Not signed in." })) && s.linked());
        assert!(!s.handle_answer(500, &serde_json::json!({ "revoked": true })) && s.linked());
        assert!(s.handle_answer(401, &serde_json::json!({ "error": "revoked", "revoked": true })));
        assert!(!s.linked() && !s.key_file().exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
