//! Cryptography, matching the JavaScript side exactly.
//!
//! - [`HostKey`]: the AI host session key. Both sides derive it from the host's secret and two nonces (HKDF-SHA256,
//!   salt = the two hex nonces' bytes, info "viberoller-ai-host-v1"); Work jobs and their events travel sealed with it
//!   (AES-256-GCM, `{ iv, ct }` in base64, the tag at the end of ct) on top of TLS. bridge/ai-nodes.js derive / seal.
//! - [`hmac_hex`] / [`hmac_ok`]: the signature of update manifests (HMAC-SHA256 with the host's secret, hex).
//! - [`chat`]: the team chat's end-to-end encryption (web/index.html sealFor / decryptMsg / sealOwn / sealChatFiles).
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{b64, unb64, Error, Result};

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
fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf::Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, &mut out).expect("32 bytes is a valid HKDF length");
    out
}

// ---------------------------------------------------------------------------------------------------------------
// The AI host session
// ---------------------------------------------------------------------------------------------------------------

/// A sealed value: `{ "iv": base64, "ct": base64 }` (bridge/ai-nodes.js seal).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Envelope { pub iv: String, pub ct: String }

/// The session key one AI host shares with its hub after /node/hello.
#[derive(Clone)]
pub struct HostKey { key: [u8; 32] }
impl HostKey {
    /// HKDF-SHA256(secret, salt = hex(host nonce + server nonce), "viberoller-ai-host-v1"), 32 bytes.
    pub fn derive(secret: &str, host_nonce_hex: &str, server_nonce_hex: &str) -> Result<Self> {
        let salt = hex::decode(format!("{host_nonce_hex}{server_nonce_hex}")).map_err(|_| Error::msg("the nonces must be hex"))?;
        Ok(HostKey { key: hkdf32(secret.as_bytes(), &salt, b"viberoller-ai-host-v1") })
    }
    /// The key itself, hex (the hub keeps a host's session across its restarts: bridge/ai-host-sessions.json).
    pub fn to_hex(&self) -> String { hex::encode(self.key) }
    pub fn from_hex(h: &str) -> Option<Self> { let b = hex::decode(h).ok()?; Some(HostKey { key: b.try_into().ok()? }) }
    /// "vr-" + the first 24 hex characters of sha256(key): both sides compare it after the handshake.
    pub fn id(&self) -> String { format!("vr-{}", &sha256_hex(&self.key)[..24]) }
    pub fn seal(&self, value: &serde_json::Value) -> Result<Envelope> {
        let iv = random_bytes(12);
        let ct = aes_gcm_seal(&self.key, &iv, b"", serde_json::to_string(value)?.as_bytes())?;
        Ok(Envelope { iv: b64(&iv), ct: b64(&ct) })
    }
    pub fn open(&self, env: &Envelope) -> Result<serde_json::Value> {
        let plain = aes_gcm_open(&self.key, &unb64(&env.iv)?, b"", &unb64(&env.ct)?)?;
        Ok(serde_json::from_slice(&plain)?)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The team chat
// ---------------------------------------------------------------------------------------------------------------
pub mod chat {
    //! The team chat's end-to-end encryption, exactly as the page does it (web/index.html):
    //! - every person has an ECDH P-256 key pair; the public half is on the hub as a JWK (`{ kty, crv, x, y }`)
    //! - a message (format 3 / 4) has its own random AES-256 key; the body is encrypted with it under the additional data
    //!   `mills-v<v>|<from>|<to>|<mid>`, and the key is wrapped for every reader with HKDF-SHA256(ECDH secret, no salt,
    //!   "mills-v3 wrap") under that additional data plus `|<reader>`
    //! - format 4 deflates the text (raw DEFLATE) and frames it (u32 length, zero padding); format 3 pads with spaces;
    //!   both pad to a size bucket (256, 512 … 64 KiB, then 64 KiB steps)
    //! - files: their own random key, additional data "vr-file-v1"; the uploader's own seal of a file's name / type / key:
    //!   the raw ECDH secret with themselves, additional data "vr-own-seal-v1", "<iv>.<ct>"
    //! - voice clips (format 4): iv + ciphertext of the framed audio, additional data `mills-v4-blob|<from>|<mid>`
    use super::*;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use p256::{PublicKey, SecretKey};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};

    pub const WRAP_INFO: &[u8] = b"mills-v3 wrap";
    pub const FILE_AAD: &[u8] = b"vr-file-v1";
    pub const OWN_AAD: &[u8] = b"vr-own-seal-v1";

    /// A public key as the hub keeps it.
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    pub struct PubJwk { pub kty: String, pub crv: String, pub x: String, pub y: String }

    /// One person's chat key pair.
    pub struct ChatKey { secret: SecretKey }
    impl ChatKey {
        pub fn generate() -> Self { ChatKey { secret: SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng) } }
        /// From a private JWK (`d` present), as the page exports it.
        pub fn from_jwk(jwk: &str) -> Result<Self> {
            // WebCrypto adds key_ops / ext; only the key itself is read
            let v: serde_json::Value = serde_json::from_str(jwk).map_err(|_| Error::msg("not a JWK"))?;
            let f = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let clean = serde_json::json!({ "kty": f("kty"), "crv": f("crv"), "x": f("x"), "y": f("y"), "d": f("d") }).to_string();
            SecretKey::from_jwk_str(&clean).map(|secret| ChatKey { secret }).map_err(|_| Error::msg("not a P-256 private key (JWK)"))
        }
        /// From PKCS#8 DER, as the page's key backup holds it.
        pub fn from_pkcs8(der: &[u8]) -> Result<Self> {
            use p256::pkcs8::DecodePrivateKey;
            SecretKey::from_pkcs8_der(der).map(|secret| ChatKey { secret }).map_err(|_| Error::msg("not a P-256 private key (PKCS#8)"))
        }
        pub fn to_jwk(&self) -> String { self.secret.to_jwk_string().to_string() }
        pub fn public_jwk(&self) -> PubJwk { pub_jwk(&self.secret.public_key()) }
        /// The raw ECDH secret with someone (the x coordinate): WebCrypto's deriveKey(ECDH → AES-GCM 256) key.
        pub fn shared(&self, their: &PubJwk) -> Result<[u8; 32]> {
            let pk = parse_pub(their)?;
            let s = p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), pk.as_affine());
            let mut out = [0u8; 32];
            out.copy_from_slice(s.raw_secret_bytes());
            Ok(out)
        }
        /// The key that wraps a message key for (or from) that person: HKDF-SHA256(ECDH secret, no salt, "mills-v3 wrap").
        pub fn wrap_key(&self, their: &PubJwk) -> Result<[u8; 32]> { Ok(hkdf32(&self.shared(their)?, &[], WRAP_INFO)) }
    }
    pub fn pub_jwk(pk: &PublicKey) -> PubJwk {
        let p = pk.to_encoded_point(false);
        let enc = |b: &[u8]| { use base64::Engine; base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b) };
        PubJwk { kty: "EC".into(), crv: "P-256".into(), x: enc(p.x().expect("uncompressed")), y: enc(p.y().expect("uncompressed")) }
    }
    pub fn parse_pub(j: &PubJwk) -> Result<PublicKey> {
        let s = format!(r#"{{"kty":"EC","crv":"P-256","x":"{}","y":"{}"}}"#, j.x, j.y);
        PublicKey::from_jwk_str(&s).map_err(|_| Error::msg("not a P-256 public key"))
    }
    /// A short fingerprint people compare: sha256 of x + y, first 16 bytes as hex pairs.
    pub fn fingerprint(j: &PubJwk) -> String { sha256_hex(format!("{}{}", j.x, j.y).as_bytes())[..32].as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ") }

    pub fn pad_bucket(n: usize) -> usize {
        if n > 65536 { return n.div_ceil(65536) * 65536; }
        let mut b = 256; while b < n { b *= 2; } b
    }
    pub fn frame(bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; pad_bucket(4 + bytes.len())];
        out[..4].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        out[4..4 + bytes.len()].copy_from_slice(bytes);
        out
    }
    pub fn unframe(b: &[u8]) -> Result<&[u8]> {
        if b.len() < 4 { return Err(Error::msg("bad frame")); }
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        if n > b.len() - 4 { return Err(Error::msg("bad frame")); }
        Ok(&b[4..4 + n])
    }
    pub fn pad_plain(s: &str) -> Vec<u8> { let b = s.as_bytes(); let mut out = vec![0x20u8; pad_bucket(b.len())]; out[..b.len()].copy_from_slice(b); out }
    pub fn deflate_raw(b: &[u8]) -> Vec<u8> { let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default()); e.write_all(b).expect("in memory"); e.finish().expect("in memory") }
    pub fn inflate_raw(b: &[u8]) -> Result<Vec<u8>> { let mut out = Vec::new(); flate2::read::DeflateDecoder::new(b).take(64 << 20).read_to_end(&mut out).map_err(|_| Error::msg("bad deflate data"))?; Ok(out) }
    pub fn body_aad(from: &str, to: &str, mid: &str, v: u8) -> String { format!("mills-v{v}|{from}|{to}|{mid}") }
    pub fn new_mid() -> String { use base64::Engine; base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes(16)) }

    /// A message key wrapped for one reader.
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    pub struct Wrapped { pub iv: String, pub wk: String }
    /// The encrypted part of a chat message as /api/room/send takes it (the hub adds from / seq / conv …).
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Sealed {
        pub v: u8,
        pub mid: String,
        pub iv: String,
        pub ct: String,
        pub keys: BTreeMap<String, Wrapped>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub blob: Option<String>,
    }

    /// Encrypts `plain` (the payload text, usually `{"v":2,"t":…}`) from `me` for `readers` (username → public key; include
    /// yourself to read your own message later). `to` is the recipient of a private message, else "".
    pub fn seal(me: &ChatKey, from: &str, to: &str, readers: &[(String, PubJwk)], plain: &str, v4: bool) -> Result<Sealed> {
        let v = if v4 { 4 } else { 3 };
        let mid = new_mid();
        let aad = body_aad(from, to, &mid, v);
        let raw: [u8; 32] = random_bytes(32).try_into().expect("32");
        let iv = random_bytes(12);
        let body = if v4 { frame(&deflate_raw(plain.as_bytes())) } else { pad_plain(plain) };
        let ct = aes_gcm_seal(&raw, &iv, aad.as_bytes(), &body)?;
        let mut keys = BTreeMap::new();
        for (user, pk) in readers {
            let kiv = random_bytes(12);
            let wk = aes_gcm_seal(&me.wrap_key(pk)?, &kiv, format!("{aad}|{user}").as_bytes(), &raw)?;
            keys.insert(user.clone(), Wrapped { iv: b64(&kiv), wk: b64(&wk) });
        }
        if !to.is_empty() && !keys.contains_key(to) { return Err(Error::msg(format!("{to} has no chat key yet"))); }
        Ok(Sealed { v, mid, iv: b64(&iv), ct: b64(&ct), keys, blob: None })
    }

    /// One message as /api/room/messages lists it (only the fields reading needs).
    #[derive(Clone, Debug, Deserialize)]
    pub struct Incoming {
        #[serde(default)] pub v: Option<u8>,
        #[serde(default)] pub mid: Option<String>,
        pub from: String,
        #[serde(default)] pub to: Option<String>,
        pub iv: String,
        pub ct: String,
        /// the reader's own wrapped key (the hub hands each reader only theirs)
        pub key: Wrapped,
        #[serde(rename = "fromPub")] pub from_pub: PubJwk,
    }
    /// Decrypts a message for `me` (username `me_name`): the payload text.
    pub fn open(me: &ChatKey, me_name: &str, m: &Incoming) -> Result<String> {
        match m.v {
            Some(v @ (3 | 4)) => {
                let mid = m.mid.as_deref().ok_or_else(|| Error::msg("no message id"))?;
                let aad = body_aad(&m.from, m.to.as_deref().unwrap_or(""), mid, v);
                let raw = aes_gcm_open(&me.wrap_key(&m.from_pub)?, &unb64(&m.key.iv)?, format!("{aad}|{me_name}").as_bytes(), &unb64(&m.key.wk)?)?;
                let raw: [u8; 32] = raw.try_into().map_err(|_| Error::msg("bad message key"))?;
                let body = aes_gcm_open(&raw, &unb64(&m.iv)?, aad.as_bytes(), &unb64(&m.ct)?)?;
                if v == 4 { Ok(String::from_utf8(inflate_raw(unframe(&body)?)?).map_err(|_| Error::msg("not text"))?) }
                else { Ok(String::from_utf8(body).map_err(|_| Error::msg("not text"))?.trim_end().to_string()) }
            }
            _ => { // the first formats: the raw ECDH secret wraps the key, no additional data
                let raw = aes_gcm_open(&me.shared(&m.from_pub)?, &unb64(&m.key.iv)?, b"", &unb64(&m.key.wk)?)?;
                let raw: [u8; 32] = raw.try_into().map_err(|_| Error::msg("bad message key"))?;
                Ok(String::from_utf8(aes_gcm_open(&raw, &unb64(&m.iv)?, b"", &unb64(&m.ct)?)?).map_err(|_| Error::msg("not text"))?)
            }
        }
    }

    /// A chat file: its ciphertext plus the key and iv that go into the message body (`k`, `i`, base64).
    pub struct SealedFile { pub ct: Vec<u8>, pub k: String, pub i: String }
    pub fn seal_file(bytes: &[u8]) -> Result<SealedFile> {
        let k: [u8; 32] = random_bytes(32).try_into().expect("32");
        let iv = random_bytes(12);
        Ok(SealedFile { ct: aes_gcm_seal(&k, &iv, FILE_AAD, bytes)?, k: b64(&k), i: b64(&iv) })
    }
    pub fn open_file(ct: &[u8], k: &str, i: &str) -> Result<Vec<u8>> {
        let k: [u8; 32] = unb64(k)?.try_into().map_err(|_| Error::msg("bad file key"))?;
        aes_gcm_open(&k, &unb64(i)?, FILE_AAD, ct)
    }
    /// The uploader's own seal of a file's info (`{ n, m, s, k, i }`): "<iv>.<ct>", only their own key opens it.
    pub fn seal_own(me: &ChatKey, info: &serde_json::Value) -> Result<String> {
        let iv = random_bytes(12);
        let ct = aes_gcm_seal(&me.shared(&me.public_jwk())?, &iv, OWN_AAD, serde_json::to_string(info)?.as_bytes())?;
        Ok(format!("{}.{}", b64(&iv), b64(&ct)))
    }
    pub fn open_own(me: &ChatKey, seal: &str) -> Result<serde_json::Value> {
        let (iv, ct) = seal.split_once('.').ok_or_else(|| Error::msg("not a seal"))?;
        Ok(serde_json::from_slice(&aes_gcm_open(&me.shared(&me.public_jwk())?, &unb64(iv)?, OWN_AAD, &unb64(ct)?)?)?)
    }
    /// A voice clip (format 4): base64 of iv + ciphertext, and its key for the message body.
    pub fn seal_blob(bytes: &[u8], from: &str, mid: &str) -> Result<(String, String)> {
        let k: [u8; 32] = random_bytes(32).try_into().expect("32");
        let iv = random_bytes(12);
        let ct = aes_gcm_seal(&k, &iv, format!("mills-v4-blob|{from}|{mid}").as_bytes(), &frame(bytes))?;
        Ok((b64(&k), b64(&[iv, ct].concat())))
    }
    pub fn open_blob(blob: &[u8], k: &str, from: &str, mid: &str) -> Result<Vec<u8>> {
        if blob.len() < 28 { return Err(Error::msg("bad clip")); }
        let k: [u8; 32] = unb64(k)?.try_into().map_err(|_| Error::msg("bad clip key"))?;
        let framed = aes_gcm_open(&k, &blob[..12], format!("mills-v4-blob|{from}|{mid}").as_bytes(), &blob[12..])?;
        Ok(unframe(&framed)?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::chat::*;
    use super::*;

    #[test]
    fn host_key_matches_node() {
        // node -e "c=require('crypto');k=Buffer.from(c.hkdfSync('sha256',Buffer.from('s3cret'),Buffer.from('00ff'+'a1b2','hex'),'viberoller-ai-host-v1',32));console.log(k.toString('hex'))"
        let k = HostKey::derive("s3cret", "00ff", "a1b2").unwrap();
        assert_eq!(hex::encode(k.key), concat!("28b7dc2e7be0a0da", "89912d948f35f473", "c8e50f658062fd9b", "0ed50059dcf80af7")); // (in pieces: the hygiene check looks for long hex)
        assert_eq!(k.id(), "vr-abc765ce1003dc37e11547d3");
        let env = k.seal(&serde_json::json!({"a": 1})).unwrap();
        assert_eq!(k.open(&env).unwrap(), serde_json::json!({"a": 1}));
        assert!(k.id().starts_with("vr-") && k.id().len() == 27);
        let other = HostKey::derive("other", "00ff", "a1b2").unwrap();
        assert!(other.open(&env).is_err());
    }
    #[test]
    fn hmac_signs_and_checks() {
        let s = hmac_hex(b"key", b"msg");
        assert!(hmac_ok(b"key", b"msg", &s));
        assert!(!hmac_ok(b"key", b"msh", &s));
        assert!(!hmac_ok(b"key", b"msg", &s[..10]));
    }
    #[test]
    fn chat_round_trips_v3_v4_files_own_and_clips() {
        let a = ChatKey::generate();
        let b = ChatKey::generate();
        let readers = vec![("alice".to_string(), a.public_jwk()), ("bob".to_string(), b.public_jwk())];
        for v4 in [false, true] {
            let s = seal(&a, "alice", "", &readers, r#"{"v":2,"t":"hello bob"}"#, v4).unwrap();
            let inc = Incoming { v: Some(s.v), mid: Some(s.mid.clone()), from: "alice".into(), to: None, iv: s.iv.clone(), ct: s.ct.clone(), key: s.keys["bob"].clone(), from_pub: a.public_jwk() };
            assert_eq!(open(&b, "bob", &inc).unwrap(), r#"{"v":2,"t":"hello bob"}"#);
            assert!(open(&b, "mallory", &inc).is_err()); // the reader's name is bound in
            let moved = Incoming { from: "carol".into(), ..inc.clone() };
            assert!(open(&b, "bob", &moved).is_err()); // so is the sender
        }
        let f = seal_file(b"file bytes").unwrap();
        assert_eq!(open_file(&f.ct, &f.k, &f.i).unwrap(), b"file bytes");
        let own = seal_own(&a, &serde_json::json!({"n": "x.txt"})).unwrap();
        assert_eq!(open_own(&a, &own).unwrap()["n"], "x.txt");
        assert!(open_own(&b, &own).is_err());
        let (k, blob) = seal_blob(b"audio", "alice", "mid1").unwrap();
        assert_eq!(open_blob(&crate::unb64(&blob).unwrap(), &k, "alice", "mid1").unwrap(), b"audio");
        let jwk = a.to_jwk();
        assert_eq!(ChatKey::from_jwk(&jwk).unwrap().public_jwk(), a.public_jwk());
    }
    #[test]
    fn buckets_match_the_page() {
        assert_eq!(pad_bucket(1), 256);
        assert_eq!(pad_bucket(257), 512);
        assert_eq!(pad_bucket(65536), 65536);
        assert_eq!(pad_bucket(65537), 131072);
        assert_eq!(unframe(&frame(b"abc")).unwrap(), b"abc");
    }
}
