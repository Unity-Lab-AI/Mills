//! Signed updates of the native programs, and swapping a running program's own binary.
//!
//! A release manifest is a JSON text `{ app, target, version, commit, files: [{ name, sha256, size }] }`. The hub hands
//! it out as that exact text plus signatures over its bytes:
//! - `sig`: Ed25519 with VibeRoller's release key (its public half is built into every program: [`RELEASE_PUBKEY`]), so
//!   only a release made where that key lives verifies, whichever hub hands it out;
//! - `hmac`: for AI hosts, also HMAC-SHA256 with the host's own secret (like the JavaScript host's file updates), so a
//!   host takes code only from the hub that knows its token.
//! The binary is then fetched, its sha256 compared with the manifest, it is started once with `--selftest`, and swapped
//! in: the running file is renamed to `<name>.old` (Windows allows renaming a running program, not overwriting it), the
//! new one takes its place. Starting the old one again is [`roll_back`]. Nothing here ever runs a file that did not match.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::host::ManifestFile;
use crate::{crypto, Error, Result};

/// VibeRoller's release public key (Ed25519, 32 bytes hex), built in at compile time from VR_RELEASE_PUBKEY
/// (native/build.sh sets it from the release key it signs with). Empty: this build was made without one, and accepts
/// only host updates signed with the host's own secret.
pub const RELEASE_PUBKEY: &str = match option_env!("VR_RELEASE_PUBKEY") { Some(k) => k, None => "" };

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)] pub app: String,
    #[serde(default)] pub target: String,
    #[serde(default)] pub version: String,
    #[serde(default)] pub commit: String,
    #[serde(default)] pub files: Vec<ManifestFile>,
}
/// What the hub sends: the manifest as text and its signatures.
#[derive(Clone, Debug, Deserialize)]
pub struct Signed { pub manifest: String, #[serde(default)] pub sig: String, #[serde(default)] pub hmac: String }

/// Checks the signatures and gives the manifest. `host_secret`: an AI host's token (its HMAC must match too).
/// A release key built into this program is required when there is one; without one, the HMAC alone is accepted.
pub fn verify(s: &Signed, host_secret: Option<&str>) -> Result<Manifest> {
    verify_with(s, host_secret, RELEASE_PUBKEY)
}
pub fn verify_with(s: &Signed, host_secret: Option<&str>, pubkey_hex: &str) -> Result<Manifest> {
    let mut checked = false;
    if !pubkey_hex.is_empty() {
        if !ed25519_ok(pubkey_hex, s.manifest.as_bytes(), &s.sig) { return Err(Error::msg("the release is not signed with VibeRoller's release key")); }
        checked = true;
    }
    if let Some(secret) = host_secret {
        if !crypto::hmac_ok(secret.as_bytes(), s.manifest.as_bytes(), &s.hmac) { return Err(Error::msg("the release is not signed by our hub")); }
        checked = true;
    }
    if !checked { return Err(Error::msg("nothing to check the release with (no release key built in, no host secret)")); }
    let m: Manifest = serde_json::from_str(&s.manifest)?;
    for f in &m.files {
        if f.name.is_empty() || f.name.contains(['/', '\\']) || f.name.starts_with('.') || f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) { return Err(Error::msg("the release lists a bad file")); }
    }
    Ok(m)
}
pub fn ed25519_ok(pubkey_hex: &str, msg: &[u8], sig_hex: &str) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let (Ok(pk), Ok(sig)) = (hex::decode(pubkey_hex), hex::decode(sig_hex)) else { return false };
    let (Ok(pk), Ok(sig)) = (<[u8; 32]>::try_from(pk.as_slice()), <[u8; 64]>::try_from(sig.as_slice())) else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else { return false };
    vk.verify(msg, &Signature::from_bytes(&sig)).is_ok()
}
/// Signs a manifest text with a release key (hex of the 32-byte secret): what the build machine does.
pub fn ed25519_sign(secret_hex: &str, msg: &[u8]) -> Result<String> {
    use ed25519_dalek::{Signer, SigningKey};
    let sk = <[u8; 32]>::try_from(hex::decode(secret_hex).map_err(|_| Error::msg("bad key"))?.as_slice()).map_err(|_| Error::msg("bad key"))?;
    Ok(hex::encode(SigningKey::from_bytes(&sk).sign(msg).to_bytes()))
}
/// A new release key pair: (secret hex, public hex).
pub fn ed25519_new() -> (String, String) {
    let sk = ed25519_dalek::SigningKey::generate(&mut p256::elliptic_curve::rand_core::OsRng);
    (hex::encode(sk.to_bytes()), hex::encode(sk.verifying_key().to_bytes()))
}

/// Is `a` a newer version than `b`? (dotted numbers; anything else compares as text)
pub fn newer(a: &str, b: &str) -> bool {
    let p = |s: &str| s.trim_start_matches('v').split(['.', '-', '+']).map(|x| x.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    p(a) > p(b)
}

/// The program's own path and the paths its update uses.
pub fn self_paths() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let exe = std::env::current_exe()?;
    let name = exe.file_name().ok_or_else(|| Error::msg("no program name"))?.to_string_lossy().to_string();
    Ok((exe.clone(), exe.with_file_name(format!("{name}.new")), exe.with_file_name(format!("{name}.old"))))
}
/// Puts `bytes` (already checked against the manifest) in place of the running program. The old one stays as .old.
pub fn swap_in(exe: &Path, bytes: &[u8], sha256: &str) -> Result<()> {
    if crypto::sha256_hex(bytes) != sha256 { return Err(Error::msg("the download does not match the signed release")); }
    let name = exe.file_name().ok_or_else(|| Error::msg("no program name"))?.to_string_lossy().to_string();
    let new = exe.with_file_name(format!("{name}.new"));
    let old = exe.with_file_name(format!("{name}.old"));
    std::fs::write(&new, bytes)?;
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?; }
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(&new, exe) { let _ = std::fs::rename(&old, exe); return Err(e.into()); }
    Ok(())
}
/// Starts a downloaded program once with --selftest: it must load and say so, or it is not used.
pub fn selftest(file: &Path, extra: &[&str]) -> Result<()> {
    let out = std::process::Command::new(file).arg("--selftest").args(extra).output().map_err(|e| Error::msg(format!("the new version does not start: {e}")))?;
    if !out.status.success() { return Err(Error::msg(format!("the new version does not start: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("")))); }
    Ok(())
}
/// Puts the previous version (.old) back.
pub fn roll_back(exe: &Path) -> Result<()> {
    let name = exe.file_name().ok_or_else(|| Error::msg("no program name"))?.to_string_lossy().to_string();
    let old = exe.with_file_name(format!("{name}.old"));
    if !old.exists() { return Err(Error::msg("there is no previous version to go back to")); }
    let bad = exe.with_file_name(format!("{name}.bad"));
    let _ = std::fs::remove_file(&bad);
    std::fs::rename(exe, &bad)?;
    std::fs::rename(&old, exe)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_signatures() {
        let (sk, pk) = ed25519_new();
        let m = serde_json::json!({ "app": "vr-host", "target": "x86_64-unknown-linux-gnu", "version": "1.2.3", "commit": "c", "files": [{ "name": "vr-host", "sha256": "a".repeat(64) }] }).to_string();
        let s = Signed { sig: ed25519_sign(&sk, m.as_bytes()).unwrap(), hmac: crypto::hmac_hex(b"tok", m.as_bytes()), manifest: m.clone() };
        assert_eq!(verify_with(&s, Some("tok"), &pk).unwrap().version, "1.2.3");
        assert!(verify_with(&s, Some("other"), &pk).is_err());
        assert!(verify_with(&s, None, &ed25519_new().1).is_err());
        assert!(verify_with(&s, None, "").is_err()); // nothing to check with
        let bad = Signed { manifest: m.replace("1.2.3", "9.9.9"), ..s.clone() };
        assert!(verify_with(&bad, Some("tok"), &pk).is_err());
        let evil = serde_json::json!({ "files": [{ "name": "../x", "sha256": "a".repeat(64) }] }).to_string();
        let e = Signed { sig: ed25519_sign(&sk, evil.as_bytes()).unwrap(), hmac: String::new(), manifest: evil };
        assert!(verify_with(&e, None, &pk).is_err());
    }
    #[test]
    fn versions_and_swap() {
        assert!(newer("3.10.0", "3.9.2") && !newer("3.9.2", "3.10.0") && !newer("1.0.0", "1.0.0"));
        let dir = std::env::temp_dir().join(format!("vr-swap-{}", crypto::random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("prog");
        std::fs::write(&exe, b"old").unwrap();
        assert!(swap_in(&exe, b"new", &"0".repeat(64)).is_err());
        swap_in(&exe, b"new", &crypto::sha256_hex(b"new")).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        roll_back(&exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
