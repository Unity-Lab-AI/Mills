//! Device-code sign-in (T-236): a tool (Jcode PGM …) with no browser asks `start`, shows the person a short user code,
//! polls with a long secret device code; the person, signed in on the page, approves or refuses the user code; the next
//! poll hands the tool a NEW per-device token. Device tokens are kept as sha256 only in `<root>/device-tokens.json`
//! (a new file: the JavaScript hub never reads it), work as a session for the API, and are revoked one by one.
//! Pending requests live in memory only (10 minutes at most).
use pgm_core_internal::json::{read_json, write_json_private as write_json};
use pgm_core_internal::now_ms;
use pgm_core_internal::time::iso_now;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use subtle::ConstantTimeEq;

pub const EXPIRES_S: u64 = 600;
pub const INTERVAL_S: u64 = 5;
const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
const MAX_PENDING: usize = 200;
const MAX_DEVICES_PER_USER: usize = 50;

#[derive(Clone, PartialEq)]
enum State { Pending, Approved(String), Denied }
struct Pending { user_code: String, client: String, device: String, expires: u64, last_poll: u64, state: State }

pub struct Devices {
    file: PathBuf,
    tokens: Mutex<Map<String, Value>>, // sha256(token) → { id, user, name, client, created, lastUsed, lastUsedAt }
    pending: Mutex<HashMap<String, Pending>>, // sha256(deviceCode) → request
    starts: Mutex<HashMap<String, Vec<u64>>>, // address → recent starts
    fails: Mutex<HashMap<String, (u32, u64)>>, // person → wrong user codes, blocked until
}

fn clean_code(c: &str) -> String { c.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase() }
fn short(s: &str, n: usize) -> String { s.chars().filter(|c| !c.is_control()).take(n).collect::<String>().trim().to_string() }
fn same(a: &str, b: &str) -> bool { a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes())) }
fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }
fn key(t: &str) -> String { pgm_core_internal::crypto::sha256_hex(t.as_bytes()) }

impl Devices {
    pub fn new(root: &Path) -> Devices {
        let file = root.join("device-tokens.json");
        let tokens = read_json(&file).and_then(|v| v["devices"].as_object().cloned()).unwrap_or_default();
        Devices { file, tokens: Mutex::new(tokens), pending: Mutex::default(), starts: Mutex::default(), fails: Mutex::default() }
    }
    fn save(&self) { write_json(&self.file, &json!({ "devices": Value::Object(lk(&self.tokens).clone()) })); }

    /// A tool asks to sign in: the codes (or why not). `ttl`: seconds it wants the request to live (never more than 10 minutes).
    pub fn start(&self, client: &str, device: &str, ttl: Option<u64>, ip: &str) -> Result<Value, (u16, String)> {
        let now = now_ms();
        {
            let mut s = lk(&self.starts);
            let v = s.entry(ip.to_string()).or_default();
            v.retain(|t| *t + 60_000 > now);
            if v.len() >= 10 { return Err((429, "Too many sign-in requests from here. Try again in a minute.".into())); }
            v.push(now);
            if s.len() > 500 { s.retain(|_, v| v.iter().any(|t| *t + 60_000 > now)); }
        }
        let mut p = lk(&self.pending);
        p.retain(|_, r| r.expires + 60_000 > now);
        if p.len() >= MAX_PENDING { return Err((429, "Too many sign-in requests are open. Try again in a few minutes.".into())); }
        let code = pgm_core_internal::crypto::random_hex(32);
        let user_code = loop {
            let c: String = pgm_core_internal::crypto::random_bytes(8).iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect();
            if !p.values().any(|r| r.user_code == c) { break c; }
        };
        let secs = ttl.unwrap_or(EXPIRES_S).clamp(1, EXPIRES_S);
        p.insert(key(&code), Pending { user_code: user_code.clone(), client: short(client, 40), device: short(device, 60), expires: now + secs * 1000, last_poll: 0, state: State::Pending });
        Ok(json!({ "deviceCode": code, "userCode": format!("{}-{}", &user_code[..4], &user_code[4..]), "expiresIn": secs, "interval": INTERVAL_S }))
    }

    /// The tool polls: (status, answer). Pending 428, too fast 429 slow_down, refused 403, expired or unknown 410, approved 200 with the new token.
    pub fn poll(&self, code: &str) -> (u16, Value) {
        let now = now_ms();
        let k = key(code);
        let mut p = lk(&self.pending);
        let Some(r) = p.get_mut(&k) else { return (410, json!({ "expired": true, "error": "This sign-in request is unknown or has expired." })) };
        if r.expires < now { p.remove(&k); return (410, json!({ "expired": true, "error": "This sign-in request has expired." })); }
        match r.state.clone() {
            State::Pending => {
                if r.last_poll != 0 && now < r.last_poll + INTERVAL_S * 1000 { r.last_poll = now; return (429, json!({ "error": "slow_down", "slow_down": true, "interval": INTERVAL_S + 5 })); }
                r.last_poll = now;
                (428, json!({ "pending": true, "interval": INTERVAL_S }))
            }
            State::Denied => { p.remove(&k); (403, json!({ "denied": true, "error": "The person refused this sign-in." })) }
            State::Approved(user) => {
                let (client, device) = (r.client.clone(), r.device.clone());
                p.remove(&k);
                drop(p);
                let token = pgm_core_internal::crypto::random_hex(32);
                let id = pgm_core_internal::crypto::random_hex(6);
                {
                    let mut t = lk(&self.tokens);
                    let mine: Vec<String> = t.iter().filter(|(_, v)| v["user"].as_str() == Some(&user)).map(|(k, _)| k.clone()).collect();
                    if mine.len() >= MAX_DEVICES_PER_USER { if let Some(old) = mine.iter().min_by_key(|k| t[*k]["lastUsedAt"].as_u64().unwrap_or(0)).cloned() { t.remove(&old); } }
                    t.insert(key(&token), json!({ "id": id, "user": user, "name": if device.is_empty() { client.clone() } else { device }, "client": client, "created": iso_now(), "createdAt": now, "lastUsed": Value::Null, "lastUsedAt": now }));
                }
                self.save();
                (200, json!({ "token": token, "username": user, "deviceId": id }))
            }
        }
    }

    /// The signed-in person approves or refuses a user code.
    pub fn decide(&self, user: &str, user_code: &str, approve: bool) -> Result<Value, (u16, String)> {
        let now = now_ms();
        let want = clean_code(user_code);
        {
            let f = lk(&self.fails);
            if let Some((_, until)) = f.get(user) { if *until > now { return Err((429, "Too many wrong codes. Try again in 15 minutes.".into())); } }
        }
        let mut p = lk(&self.pending);
        p.retain(|_, r| r.expires + 60_000 > now);
        let mut hit = None;
        for (k, r) in p.iter() { if same(&r.user_code, &want) { hit = Some(k.clone()); } } // every entry compared, no early exit
        let Some(k) = hit.filter(|_| want.len() == 8) else {
            let mut f = lk(&self.fails);
            let e = f.entry(user.to_string()).or_insert((0, 0));
            e.0 += 1;
            if e.0 >= 8 { *e = (0, now + 15 * 60_000); }
            return Err((404, "No such code. Check it and try again.".into()));
        };
        let r = p.get_mut(&k).expect("found");
        if r.expires < now { p.remove(&k); return Err((410, "That code has expired: start again on the device.".into())); }
        if r.state != State::Pending { return Err((409, "That code was already answered.".into())); }
        r.state = if approve { State::Approved(user.to_string()) } else { State::Denied };
        lk(&self.fails).remove(user);
        Ok(json!({ "ok": true, "approved": approve, "client": r.client, "device": r.device }))
    }

    /// Whose token is this (a device token), and note that it was used.
    pub fn user_of(&self, token: &str) -> Option<String> {
        let k = key(token);
        let now = now_ms();
        let (user, stale) = {
            let mut t = lk(&self.tokens);
            let r = t.get_mut(&k)?;
            let stale = now > r["lastUsedAt"].as_u64().unwrap_or(0) + 60_000;
            if stale { r["lastUsedAt"] = json!(now); r["lastUsed"] = json!(iso_now()); }
            (r["user"].as_str()?.to_string(), stale)
        };
        if stale { self.save(); }
        Some(user)
    }
    pub fn is_token(&self, token: &str) -> bool { lk(&self.tokens).contains_key(&key(token)) }
    /// A person's own devices (never the token), `current`: the token this request came with.
    pub fn list(&self, user: &str, current: Option<&str>) -> Vec<Value> {
        let cur = current.map(key);
        let mut v: Vec<Value> = lk(&self.tokens).iter().filter(|(_, r)| r["user"].as_str() == Some(user)).map(|(k, r)| {
            json!({ "id": r["id"], "name": r["name"], "client": r["client"], "created": r["created"], "lastUsed": r["lastUsed"], "current": cur.as_deref() == Some(k.as_str()) })
        }).collect();
        v.sort_by(|a, b| b["created"].as_str().cmp(&a["created"].as_str()));
        v
    }
    /// Revokes one of that person's devices.
    pub fn revoke(&self, user: &str, id: &str) -> bool {
        let gone = { let mut t = lk(&self.tokens); let k = t.iter().find(|(_, r)| r["user"].as_str() == Some(user) && r["id"].as_str() == Some(id)).map(|(k, _)| k.clone()); k.map(|k| t.remove(&k)).is_some() };
        if gone { self.save(); }
        gone
    }
    /// All of a person's devices go (a new password, an admin's sign-out).
    pub fn revoke_all(&self, user: &str) {
        let n = { let mut t = lk(&self.tokens); let b = t.len(); t.retain(|_, r| r["user"].as_str() != Some(user)); b - t.len() };
        if n > 0 { self.save(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf { let d = std::env::temp_dir().join(format!("vr-dev-{}", pgm_core_internal::crypto::random_hex(4))); std::fs::create_dir_all(&d).unwrap(); d }
    #[test]
    fn approve_poll_use_revoke() {
        let d = Devices::new(&root());
        let s = d.start("jcode", "laptop", None, "x").unwrap();
        let (code, uc) = (s["deviceCode"].as_str().unwrap().to_string(), s["userCode"].as_str().unwrap().to_string());
        assert_eq!(uc.len(), 9);
        assert_eq!(d.poll(&code).0, 428);
        assert_eq!(d.poll(&code).0, 429);
        assert!(d.decide("a", "WRONG-CODE", true).is_err());
        d.decide("a", &uc.to_lowercase(), true).unwrap();
        let (st, v) = d.poll(&code);
        assert_eq!(st, 200);
        let tok = v["token"].as_str().unwrap();
        assert_eq!(d.user_of(tok).as_deref(), Some("a"));
        assert_eq!(d.poll(&code).0, 410);
        assert!(d.list("b", None).is_empty());
        let id = d.list("a", Some(tok))[0]["id"].as_str().unwrap().to_string();
        assert!(!d.revoke("b", &id) && d.revoke("a", &id));
        assert!(d.user_of(tok).is_none());
    }
    #[test]
    fn deny_and_expiry() {
        let d = Devices::new(&root());
        let s = d.start("c", "", Some(1), "y").unwrap();
        let (code, uc) = (s["deviceCode"].as_str().unwrap().to_string(), s["userCode"].as_str().unwrap().to_string());
        d.decide("a", &uc, false).unwrap();
        assert_eq!(d.poll(&code).0, 403);
        let s = d.start("c", "", Some(1), "y").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert_eq!(d.poll(s["deviceCode"].as_str().unwrap()).0, 410);
    }
}
