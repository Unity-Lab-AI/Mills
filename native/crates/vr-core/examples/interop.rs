//! Interop checks against the JavaScript side (the regression suite runs this: bridge/test/regress.js R163).
//! Reads one JSON request on stdin, answers one JSON line:
//!   { "op": "host-key", "secret", "hostNonce", "serverNonce" }                     → { kid, sealed (of {"x":1}) }
//!   { "op": "host-open", "secret", "hostNonce", "serverNonce", "env": {iv, ct} }   → { value }
//!   { "op": "chat-seal", "jwk" (sender's private JWK), "from", "to", "readers": [[user, pubJwk]], "plain", "v4" } → sealed
//!   { "op": "chat-open", "jwk" (reader's private JWK), "me", "msg" }               → { plain }
//!   { "op": "own-open", "jwk", "seal" }  → { info }      { "op": "file-open", "ct" (b64), "k", "i" } → { plain (utf8) }
//!   { "op": "hmac", "key", "msg" }       → { sig }
use serde_json::{json, Value};
use std::io::Read;
use vr_core::crypto::{chat, Envelope, HostKey};

fn main() {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).expect("stdin");
    let r: Value = serde_json::from_str(&s).expect("json");
    let g = |k: &str| r[k].as_str().unwrap_or("").to_string();
    let out = (|| -> vr_core::Result<Value> {
        Ok(match g("op").as_str() {
            "host-key" => { let k = HostKey::derive(&g("secret"), &g("hostNonce"), &g("serverNonce"))?; json!({ "kid": k.id(), "sealed": k.seal(&json!({ "x": 1 }))? }) }
            "host-open" => { let k = HostKey::derive(&g("secret"), &g("hostNonce"), &g("serverNonce"))?; let env: Envelope = serde_json::from_value(r["env"].clone())?; json!({ "value": k.open(&env)? }) }
            "chat-seal" => {
                let me = chat::ChatKey::from_jwk(&r["jwk"].to_string())?;
                let readers: Vec<(String, chat::PubJwk)> = r["readers"].as_array().cloned().unwrap_or_default().into_iter().map(|p| (p[0].as_str().unwrap_or("").to_string(), serde_json::from_value(p[1].clone()).expect("pub"))).collect();
                serde_json::to_value(chat::seal(&me, &g("from"), &g("to"), &readers, &g("plain"), r["v4"].as_bool().unwrap_or(true))?)?
            }
            "chat-open" => { let me = chat::ChatKey::from_jwk(&r["jwk"].to_string())?; let m: chat::Incoming = serde_json::from_value(r["msg"].clone())?; json!({ "plain": chat::open(&me, &g("me"), &m)? }) }
            "own-open" => { let me = chat::ChatKey::from_jwk(&r["jwk"].to_string())?; json!({ "info": chat::open_own(&me, &g("seal"))? }) }
            "file-open" => json!({ "plain": String::from_utf8_lossy(&chat::open_file(&vr_core::unb64(&g("ct"))?, &g("k"), &g("i"))?) }),
            "hmac" => json!({ "sig": vr_core::crypto::hmac_hex(g("key").as_bytes(), g("msg").as_bytes()) }),
            o => return Err(vr_core::Error::msg(format!("unknown op {o}"))),
        })
    })();
    match out { Ok(v) => println!("{v}"), Err(e) => { println!("{}", json!({ "error": e.to_string() })); std::process::exit(1) } }
}
