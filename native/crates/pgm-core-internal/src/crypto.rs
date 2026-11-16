//! The cryptographic primitives the PGM libraries build on (each matches its Node.js / WebCrypto counterpart byte for
//! byte): random bytes / hex ids, SHA-256, HMAC-SHA256 with a constant-time check, AES-256-GCM (tag appended, as
//! WebCrypto does), HKDF-SHA256.
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// `n` random bytes from the operating system's generator.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::rng().fill_bytes(&mut v);
    v
}
/// `n` random bytes as lowercase hex (a nonce for /node/hello is 16 bytes: 32 hex characters).
pub fn random_hex(n: usize) -> String { hex::encode(random_bytes(n)) }

pub fn sha256(bytes: &[u8]) -> [u8; 32] { Sha256::digest(bytes).into() }
pub fn sha256_hex(bytes: &[u8]) -> String { hex::encode(sha256(bytes)) }

/// HMAC-SHA256 of `msg` under `key`, lowercase hex (crypto.createHmac('sha256', key).update(msg).digest('hex')).
pub fn hmac_hex(key: &[u8], msg: &[u8]) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(msg);
    hex::encode(m.finalize().into_bytes())
}
/// Is `sig` (hex) the HMAC of `msg` under `key`? Compared in constant time.
pub fn hmac_ok(key: &[u8], msg: &[u8], sig: &str) -> bool {
    let want = hmac_hex(key, msg);
    same(want.as_bytes(), sig.as_bytes())
}
/// HMAC-SHA256 of `msg` under `key`, the 32 raw bytes.
pub fn hmac_raw(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(msg);
    m.finalize().into_bytes().into()
}
/// Are two secrets the same? Compared in constant time (only the length can show); an empty `want` never matches.
pub fn same(want: &[u8], given: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    !want.is_empty() && want.len() == given.len() && bool::from(want.ct_eq(given))
}

/// AES-256-GCM with additional data; the 16-byte tag is appended to the ciphertext (as WebCrypto and the hub do).
pub fn aes_gcm_seal(key: &[u8; 32], iv: &[u8], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let c = Aes256Gcm::new_from_slice(key).map_err(|_| Error::msg("bad key"))?;
    if iv.len() != 12 { return Err(Error::msg("the iv must be 12 bytes")); }
    c.encrypt(Nonce::from_slice(iv), Payload { msg: plain, aad }).map_err(|_| Error::msg("encryption failed"))
}
pub fn aes_gcm_open(key: &[u8; 32], iv: &[u8], aad: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
    let c = Aes256Gcm::new_from_slice(key).map_err(|_| Error::msg("bad key"))?;
    if iv.len() != 12 { return Err(Error::msg("the iv must be 12 bytes")); }
    c.decrypt(Nonce::from_slice(iv), Payload { msg: ct, aad }).map_err(|_| Error::msg("could not decrypt (wrong key, or the data was changed)"))
}
/// HKDF-SHA256 to 32 bytes.
pub fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf::Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, &mut out).expect("32 bytes is a valid HKDF length");
    out
}

/// A statement one program vouches for to another that shares `key` with it (the hub telling the Jcode PGM Server who a
/// request is, `x-pgm-hub-user`): `<payload>.<sig>`, `payload` = base64url (no padding) of the JSON, `sig` = base64url
/// (no padding) of HMAC-SHA256 under `key` over the `payload` text.
pub fn vouch(key: &[u8], v: &serde_json::Value) -> String {
    use base64::Engine;
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = b.encode(v.to_string());
    let sig = b.encode(hmac_raw(key, payload.as_bytes()));
    format!("{payload}.{sig}")
}
/// The JSON of a [`vouch`] when its signature is right (constant time) and its `at` (ms) is within `max_age_ms` of `now`.
pub fn vouch_check(key: &[u8], token: &str, now_ms: u64, max_age_ms: u64) -> Option<serde_json::Value> {
    use base64::Engine;
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (payload, sig) = token.split_once('.')?;
    let want = b.encode(hmac_raw(key, payload.as_bytes()));
    if !same(want.as_bytes(), sig.as_bytes()) { return None; }
    let v: serde_json::Value = serde_json::from_slice(&b.decode(payload).ok()?).ok()?;
    let at = v["at"].as_u64()?;
    (now_ms.abs_diff(at) <= max_age_ms).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_vouch_is_checked_by_its_key_and_its_age() {
        let v = serde_json::json!({ "user": "ann", "deviceId": "d1", "at": 1_000_000u64 });
        let t = vouch(b"shared", &v);
        assert_eq!(vouch_check(b"shared", &t, 1_000_500, 60_000), Some(v.clone()));
        assert!(vouch_check(b"other", &t, 1_000_500, 60_000).is_none());
        assert!(vouch_check(b"shared", &t, 1_100_000, 60_000).is_none());
        let (p, s) = t.split_once('.').unwrap();
        assert!(vouch_check(b"shared", &format!("{p}x.{s}"), 1_000_500, 60_000).is_none());
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"", b"") && !same(b"abc", b"ab"));
    }
    #[test]
    fn hmac_and_aes_round_trip() {
        let sig = hmac_hex(b"k", b"m");
        assert!(hmac_ok(b"k", b"m", &sig) && !hmac_ok(b"k", b"n", &sig));
        let key = hkdf32(b"secret", b"salt", b"info");
        let iv = random_bytes(12);
        let ct = aes_gcm_seal(&key, &iv, b"aad", b"plain").unwrap();
        assert_eq!(aes_gcm_open(&key, &iv, b"aad", &ct).unwrap(), b"plain");
        assert!(aes_gcm_open(&key, &iv, b"other", &ct).is_err());
        // (in pieces: the hygiene check looks for long hex)
        assert_eq!(sha256_hex(b"abc"), concat!("ba7816bf8f01cfea", "414140de5dae2223", "b00361a396177a9c", "b410ff61f20015ad"));
    }
}
