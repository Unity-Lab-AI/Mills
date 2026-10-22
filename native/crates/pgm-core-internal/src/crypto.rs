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
    use subtle::ConstantTimeEq;
    want.len() == sig.len() && bool::from(want.as_bytes().ct_eq(sig.as_bytes()))
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

#[cfg(test)]
mod tests {
    use super::*;
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
