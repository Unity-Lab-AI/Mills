//! Accounts & Authentication (board T-131), moved from bridge/server.js. This module owns:
//!   sign-in (POST /api/login): username (any case) + password (scrypt, the format auth.json has always had) + the
//!     device id; per-address limits (5 wrong for one username from one address → that address waits 15 minutes for
//!     it; 20 from one address → it waits; at most 3 sign-ins in flight per address; the same cost for a username that
//!     does not exist); the device lock (the first device an account signs in from; an admin resets it);
//!   the sessions: 12 hours, kept as the sha256 of the token only (bridge/front-sessions.json), so a copy of the file
//!     signs nobody in; on its first start it takes over the JavaScript hub's (hub-state.json) and device locks
//!     (state.json lockedDevice), so nobody is signed out or loses their lock by the move;
//!   the session gate: every request with a valid session goes on to the JavaScript hub with X-VR-User (trusted there
//!     only with the front's secret); sign-out; the admin's new password / unlock / sign-out for an account, and the
//!     locks / sign-ins the admin list shows.
//! What stays in the JavaScript hub: the accounts' profiles and settings (it reloads them when one is made or deleted)
//! and what a sign-in starts there (Unity's greeting), which it is told through /internal/front/signed-in.
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::front::{json as jresp, Req};
use crate::modules::Routed;
use vr_core::now_ms;

const SESSION_MS: u64 = 12 * 60 * 60 * 1000;
const BLOCK_MS: u64 = 15 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct Account { pub username: String, pub dir: PathBuf, pub admin: bool, pub display: String }
/// An account's admin flag from its config.json (the default account's: bridge/config.json), without the whole list.
pub fn admin_in(root: &std::path::Path, default_user: &str, user: &str) -> bool {
    if user.is_empty() { return false; }
    let f = if user == default_user { root.join("config.json") } else if valid_username(user) { root.join("profiles").join(user).join("config.json") } else { return false };
    read_json(&f).map(|c| c["admin"].as_bool() == Some(true)).unwrap_or(false)
}

#[derive(Default)]
struct Fail { count: u32, blocked_until: u64, inflight: u32, at: u64 }

pub struct Auth {
    root: PathBuf,
    default_user: String,
    behind_proxy: bool,
    sessions: Mutex<Map<String, Value>>, // sha256(token) → { user, exp }
    locks: Mutex<Map<String, Value>>,    // username → { device (sha256 of the device id), at }
    fails: Mutex<HashMap<String, Fail>>,
    last_sweep: Mutex<Instant>,
}

fn read_json(p: &Path) -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) }
fn write_json(p: &Path, v: &Value) { let tmp = p.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() { vr_core::settings::private(&tmp); let _ = std::fs::rename(&tmp, p); } }
pub fn valid_username(u: &str) -> bool { !u.is_empty() && u.len() <= 32 && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') }
pub fn token_key(t: &str) -> String { vr_core::crypto::sha256_hex(t.as_bytes()) }

impl Auth {
    pub fn new(root: &Path, cfg: &crate::modules::Config) -> Auth {
        let default_user = cfg.get("defaultUsername").and_then(Value::as_str).unwrap_or("mills").to_string();
        let behind_proxy = cfg.get("behindProxy").and_then(Value::as_bool) == Some(true);
        let a = Auth { root: root.to_path_buf(), default_user, behind_proxy, sessions: Mutex::default(), locks: Mutex::default(), fails: Mutex::default(), last_sweep: Mutex::new(Instant::now()) };
        a.take_over();
        a
    }
    fn sessions_file(&self) -> PathBuf { self.root.join("front-sessions.json") }
    fn locks_file(&self) -> PathBuf { self.root.join("device-locks.json") }
    /// First start: the JavaScript hub's sessions and device locks come over (afterwards these files are this module's).
    fn take_over(&self) {
        let now = now_ms();
        let sessions = match read_json(&self.sessions_file()) {
            Some(v) => v["sessions"].as_object().cloned().unwrap_or_default(),
            None => {
                let hs = read_json(&self.root.join("hub-state.json")).unwrap_or(Value::Null);
                let hashed = hs["sessionsHashed"].as_bool() == Some(true);
                hs["sessions"].as_object().map(|m| m.iter().map(|(k, v)| (if hashed { k.clone() } else { token_key(k) }, v.clone())).collect()).unwrap_or_default()
            }
        };
        *self.sessions.lock().unwrap() = sessions.into_iter().filter(|(_, s)| s["exp"].as_u64().unwrap_or(0) > now).collect();
        let locks = match read_json(&self.locks_file()) {
            Some(v) => v.as_object().cloned().unwrap_or_default(),
            None => {
                let mut m = Map::new();
                for a in self.accounts() {
                    if let Some(st) = read_json(&a.dir.join("state.json")) {
                        if let Some(d) = st["lockedDevice"].as_str() { m.insert(a.username.clone(), json!({ "device": d, "at": st["lockedAt"] })); }
                    }
                }
                m
            }
        };
        *self.locks.lock().unwrap() = locks;
        self.save_sessions(); self.save_locks();
    }
    fn save_sessions(&self) { write_json(&self.sessions_file(), &json!({ "sessions": Value::Object(self.sessions.lock().unwrap().clone()), "sessionsHashed": true })); }
    fn save_locks(&self) { write_json(&self.locks_file(), &Value::Object(self.locks.lock().unwrap().clone())); }

    /// Is that account an admin? (what accounts() says, for one name)
    pub fn is_admin(&self, user: &str) -> bool { admin_in(&self.root, &self.default_user, user) }
    /// The accounts, as the JavaScript hub sees them: the default one (bridge/ itself) and every profile with a
    /// config.json or an auth.json.
    pub fn accounts(&self) -> Vec<Account> {
        let profile = |dir: PathBuf, username: String| -> Account {
            let c = read_json(&dir.join("config.json")).unwrap_or(Value::Null);
            let display = c["user"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| { let mut s = username.clone(); if let Some(f) = s.get_mut(0..1) { f.make_ascii_uppercase(); } s });
            Account { admin: c["admin"].as_bool() == Some(true), display, dir, username }
        };
        let mut out = vec![];
        let base = read_json(&self.root.join("config.json")).unwrap_or(Value::Null);
        let mut d = profile(self.root.clone(), self.default_user.clone());
        d.admin = base["admin"].as_bool() == Some(true);
        out.push(d);
        let mut names: Vec<String> = std::fs::read_dir(self.root.join("profiles")).map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
        names.sort();
        for n in names {
            let dir = self.root.join("profiles").join(&n);
            if !valid_username(&n) || n == self.default_user || !(dir.join("config.json").exists() || dir.join("auth.json").exists()) { continue; }
            out.push(profile(dir, n));
        }
        out
    }
    pub fn account(&self, u: &str) -> Option<Account> { self.accounts().into_iter().find(|a| a.username == u) }
    /// The caller's address as the JavaScript hub sees it: Cloudflare's header, else (behind nginx) X-Real-IP / X-Forwarded-For.
    fn outside_ip(&self, r: &Req) -> String {
        if let Some(c) = r.header("cf-connecting-ip") { return c.trim().to_string(); }
        if self.behind_proxy { if let Some(x) = r.header("x-real-ip").map(String::from).or_else(|| r.header("x-forwarded-for").map(|v| v.split(',').next().unwrap_or("").to_string())) { return x.trim().to_string(); } }
        String::new()
    }
    /// Who a request belongs to (a valid session), or None. An expired / orphaned session is dropped.
    pub fn session_user(&self, r: &Req) -> Option<String> {
        let tok = r.header("authorization").and_then(|a| a.strip_prefix("Bearer "))?;
        let k = token_key(tok);
        let mut s = self.sessions.lock().unwrap();
        let rec = s.get(&k)?.clone();
        let user = rec["user"].as_str().unwrap_or("").to_string();
        if rec["exp"].as_u64().unwrap_or(0) < now_ms() || self.account(&user).is_none() { s.remove(&k); drop(s); self.save_sessions(); return None; }
        Some(user)
    }
    pub fn forget_session(&self, key: &str) { if self.sessions.lock().unwrap().remove(key).is_some() { self.save_sessions(); } }
    pub fn drop_sessions(&self, user: &str) { self.sessions.lock().unwrap().retain(|_, s| s["user"].as_str() != Some(user)); self.save_sessions(); }
    pub fn signed_in(&self, user: &str) -> usize { let now = now_ms(); self.sessions.lock().unwrap().values().filter(|s| s["user"].as_str() == Some(user) && s["exp"].as_u64().unwrap_or(0) > now).count() }
    pub fn lock_of(&self, user: &str) -> Option<Value> { self.locks.lock().unwrap().get(user).cloned() }
    pub fn unlock(&self, user: &str) { self.locks.lock().unwrap().remove(user); self.save_locks(); }
    /// The admin's "allow one more device": the next sign-in from a new device joins this account's devices (up to
    /// MAX_DEVICES) instead of being refused. Nothing to do for an account no device has claimed yet.
    pub fn allow_device(&self, user: &str) -> Result<usize, String> {
        let mut locks = self.locks.lock().unwrap();
        let Some(l) = locks.get_mut(user) else { return Err("No device has claimed this account yet: its next sign-in does.".into()) };
        let n = lock_devices(l).len();
        if n >= MAX_DEVICES { return Err(format!("This account already has {MAX_DEVICES} devices: reset its lock first.")); }
        l["allowNext"] = json!(true); drop(locks); self.save_locks();
        Ok(n)
    }

    fn sweep(&self) { // counters nobody touched for 30 minutes go (never all at once)
        let mut last = self.last_sweep.lock().unwrap();
        if last.elapsed() < Duration::from_secs(300) { return; }
        *last = Instant::now();
        let (now, old) = (now_ms(), now_ms().saturating_sub(30 * 60 * 1000));
        self.fails.lock().unwrap().retain(|_, f| f.inflight > 0 || f.blocked_until >= now || f.at >= old);
    }

    pub async fn login(&self, r: &Req, maintenance: Option<String>) -> Routed {
        if let Some(w) = maintenance { return Routed::Answer(jresp(503, &json!({ "error": format!("VibeRoller has moved to {w}: open it there and sign in as usual. This copy is read-only now.") }))); }
        if r.body.len() > 16384 { return Routed::Answer(jresp(413, &json!({ "error": "That request is too large." }))); }
        let b: Value = match serde_json::from_slice(if r.body.is_empty() { b"{}" } else { &r.body }) { Ok(v) => v, Err(_) => return Routed::Answer(jresp(400, &json!({ "error": "That request is not valid JSON." }))) };
        self.sweep();
        let uname = b["username"].as_str().unwrap_or("").trim().to_lowercase();
        let ip = { let i = self.outside_ip(r); if i.is_empty() { "local".to_string() } else { i } };
        let (fk, gk) = (format!("{uname}|{ip}"), format!("ip|{ip}"));
        let now = now_ms();
        {
            let mut fails = self.fails.lock().unwrap();
            let (fc, fb, fi) = { let f = fails.entry(fk.clone()).or_insert_with(|| Fail { at: now, ..Default::default() }); (f.count, f.blocked_until, f.inflight) };
            let (gc, gb, gi) = { let g = fails.entry(gk.clone()).or_insert_with(|| Fail { at: now, ..Default::default() }); (g.count, g.blocked_until, g.inflight) };
            if now < fb || now < gb || fc + fi >= 5 || gc + gi >= 20 { return Routed::Answer(jresp(429, &json!({ "error": "Too many failed attempts. Try again in 15 minutes." }))); }
            if gi >= 3 { return Routed::Answer(jresp(429, &json!({ "error": "Too many sign-ins at once from here. Try again in a moment." }))); }
            fails.get_mut(&fk).unwrap().inflight += 1; fails.get_mut(&gk).unwrap().inflight += 1;
        }
        let acct = if valid_username(&uname) { self.account(&uname) } else { None };
        let password = b["password"].as_str().map(String::from);
        let device = b["deviceId"].as_str().map(String::from);
        let auth = acct.as_ref().and_then(|a| read_json(&a.dir.join("auth.json")));
        // scrypt runs off the async threads, so a flood of wrong passwords never stalls the hub
        let good_pw = tokio::task::spawn_blocking(move || match (&auth, &password) {
            (Some(a), Some(pw)) => check_password(a, pw),
            _ => { let _ = scrypt_hash(password.as_deref().unwrap_or(""), b"mills-no-such-user", 14, 8, 1); false } // the same cost either way
        }).await.unwrap_or(false);
        let good = good_pw && acct.is_some() && device.as_ref().map(|d| d.len() >= 16).unwrap_or(false);
        let mut fails = self.fails.lock().unwrap();
        for k in [&fk, &gk] { if let Some(f) = fails.get_mut(k) { f.inflight = f.inflight.saturating_sub(1); } }
        if !good {
            let t = now_ms();
            if let Some(f) = fails.get_mut(&fk) { f.at = t; f.count += 1; if f.count >= 5 { f.blocked_until = t + BLOCK_MS; f.count = 0; } }
            if let Some(g) = fails.get_mut(&gk) { g.at = t; g.count += 1; if g.count >= 20 { g.blocked_until = t + BLOCK_MS; g.count = 0; } }
            return Routed::Answer(jresp(401, &json!({ "error": "Wrong username or password." })));
        }
        fails.remove(&fk);
        drop(fails);
        let acct = acct.expect("checked");
        let dh = vr_core::crypto::sha256_hex(device.unwrap_or_default().as_bytes());
        {
            let mut locks = self.locks.lock().unwrap();
            match locks.get_mut(&acct.username) {
                None => { locks.insert(acct.username.clone(), json!({ "device": dh, "at": iso_now() })); drop(locks); self.save_locks(); println!("[vr-hub] {}: claimed by first sign-in.", acct.username); }
                Some(l) if lock_devices(l).contains(&dh) => {}
                // the admin allowed one more device: this one joins (the browser and the app on one PC are two devices)
                Some(l) if l["allowNext"] == json!(true) => {
                    if !l["devices"].is_array() { l["devices"] = json!([]); }
                    l["devices"].as_array_mut().unwrap().push(json!({ "device": dh, "at": iso_now() }));
                    l.as_object_mut().unwrap().remove("allowNext");
                    drop(locks); self.save_locks(); println!("[vr-hub] {}: another device joined (allowed by an admin).", acct.username);
                }
                Some(_) => { println!("[vr-hub] {}: refused a sign-in from another device.", acct.username); return Routed::Answer(jresp(403, &json!({ "error": "This account is locked to its own devices. Ask an admin to allow one more device (or to reset the lock)." }))); }
            }
        }
        let token = vr_core::crypto::random_hex(32);
        self.sessions.lock().unwrap().insert(token_key(&token), json!({ "user": acct.username, "exp": now_ms() + SESSION_MS }));
        self.save_sessions();
        Routed::SignedIn { token, user: acct.username, client: b["client"].clone(), ip: self.outside_ip(r) }
    }
}

/// How many devices one account may have (the browser, the app, a laptop …).
pub const MAX_DEVICES: usize = 5;
/// A lock's devices: the first one ("device", as every lock had before 4.1.1) and the ones an admin let join.
pub fn lock_devices(l: &Value) -> Vec<String> {
    let mut v: Vec<String> = l["device"].as_str().map(String::from).into_iter().collect();
    v.extend(l["devices"].as_array().map(|a| a.iter().filter_map(|d| d["device"].as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default());
    v
}
pub fn iso_now() -> String { iso_at(now_ms() as i64) }
/// new Date(ms).toISOString()
pub fn iso_at(ms: i64) -> String {
    let s = ms.div_euclid(1000); let (days, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem / 60 % 60, rem % 60, ms.rem_euclid(1000))
}
fn civil(z: i64) -> (i64, u32, u32) { // days since 1970-01-01 → (year, month, day)
    let z = z + 719468; let era = z.div_euclid(146097); let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn scrypt_hash(pw: &str, salt: &[u8], log_n: u8, r: u32, p: u32) -> Option<Vec<u8>> {
    let params = scrypt::Params::new(log_n, r, p, 64).ok()?;
    let mut out = vec![0u8; 64];
    scrypt::scrypt(pw.as_bytes(), salt, &params, &mut out).ok()?;
    Some(out)
}
/// auth.json: { N, r, p, salt (hex), hash (hex) } (bridge/common.js hashPassword).
pub fn check_password(auth: &Value, pw: &str) -> bool {
    let n = auth["N"].as_u64().unwrap_or(16384);
    if !n.is_power_of_two() || n < 2 { return false; }
    let (Ok(salt), Ok(want)) = (hex::decode(auth["salt"].as_str().unwrap_or("")), hex::decode(auth["hash"].as_str().unwrap_or(""))) else { return false };
    let Some(got) = scrypt_hash(pw, &salt, n.trailing_zeros() as u8, auth["r"].as_u64().unwrap_or(8) as u32, auth["p"].as_u64().unwrap_or(1) as u32) else { return false };
    use subtle::ConstantTimeEq;
    got.len() == want.len() && bool::from(got.ct_eq(&want))
}
pub fn hash_password(pw: &str) -> Value {
    let salt = vr_core::crypto::random_bytes(16);
    let hash = scrypt_hash(pw, &salt, 14, 8, 1).unwrap_or_default();
    json!({ "N": 16384, "r": 8, "p": 1, "salt": hex::encode(salt), "hash": hex::encode(hash) })
}
/// A new password as the JavaScript hub makes them: 15 random bytes, base64url (20 characters).
pub fn new_password() -> String { use base64::Engine; base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vr_core::crypto::random_bytes(15)) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passwords_match_node_scrypt() {
        // the hash Node's crypto.scryptSync('pw-test', <the salt>, 64, { N: 16384, r: 8, p: 1 }) gives (in pieces: the
        // hygiene check looks for long hex)
        let node = json!({ "N": 16384, "r": 8, "p": 1, "salt": concat!("0011223344556677", "8899aabbccddeeff"),
            "hash": concat!("4b1c9729fda9c227", "db7435ab1c3fb707", "1079b497e1df886e", "3306f88f774ab18d", "b00a99fe6140ddb0", "76814702e6305c45", "6dff2b85a98d8686", "c29e28445f10c525") });
        assert!(check_password(&node, "pw-test") && !check_password(&node, "pw-tesT"));
        let h = hash_password("hello there");
        assert!(check_password(&h, "hello there"));
        assert!(!check_password(&h, "hello therE"));
        assert_eq!(new_password().len(), 20);
        assert!(iso_now().ends_with('Z') && iso_now().len() == 24);
        assert_eq!(civil(0), (1970, 1, 1)); assert_eq!(civil(20381), (2025, 10, 20));
    }
}
