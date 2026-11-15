//! Browser sign-in for Jcode PGM (T-371, plans/pgm-accounts-keys-app.md section 1): an authorization code with PKCE and a
//! loopback redirect, next to the device-code sign-in (devices.rs) and ending in the same kind of per-device token.
//!
//! The tool opens `/app-signin?client&device&redirect&state&challenge` in the person's browser; the page (signed in as
//! the person) asks `check` who is asking, then `authorize` makes a one-time code and answers where to send the browser
//! (`redirect?code=..&state=..`); the tool trades the code and its PKCE verifier for the token with `exchange`.
//! Codes live in memory only (a restart loses the open ones: the person just starts again). A code is 32 random bytes
//! (hex), valid 120 seconds (config `appSignin.codeSeconds`), single use, bound to the person, the challenge, the
//! redirect, the client and the device. Any failed presentation burns the code; a code presented a second time revokes
//! the token its first exchange made. Neither the code, the verifier nor the token ever goes into the security log.
use crate::devices::{key, lk, same, Devices};
use pgm_core_internal::audit;
use pgm_core_internal::now_ms;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// The clients that may ask for a browser sign-in.
pub const CLIENTS: &[&str] = &["jcodepgm", "jcodepgm-desktop"];
pub const CODE_SECONDS: u64 = 120;
const MAX_OPEN: usize = 300;
const MAX_KEPT: usize = 2000;
/// A used code is remembered this long after it would have expired, so that a replay still revokes the token it made.
const KEEP_AFTER_MS: u64 = 10 * 60_000;
/// Per address and minute.
const EXCHANGES_PER_MINUTE: usize = 10;
const AUTHORIZES_PER_MINUTE: usize = 30;
const CHECKS_PER_MINUTE: usize = 60;

struct Code { user: String, challenge: String, redirect: String, client: String, device: String, expires: u64, state: Life }
enum Life { Open, Taking, Used { id: String }, Burnt }

pub struct State { codes: Mutex<HashMap<String, Code>>, seconds: AtomicU64 }
impl Default for State { fn default() -> State { State { codes: Mutex::default(), seconds: AtomicU64::new(CODE_SECONDS) } } }

// ----- the validators (pure; table-driven tests below) -----

/// `http://127.0.0.1:<port>/cb` or `http://localhost:<port>/cb`, the port 1024-65535 written without leading zeros: nothing
/// else (no credentials, query, fragment, other path, other scheme or host).
pub fn valid_redirect(s: &str) -> Result<(), &'static str> {
    const BAD: &str = "The redirect must be http://127.0.0.1:<port>/cb or http://localhost:<port>/cb (port 1024 to 65535).";
    let rest = s.strip_prefix("http://127.0.0.1:").or_else(|| s.strip_prefix("http://localhost:")).ok_or(BAD)?;
    let port = rest.strip_suffix("/cb").ok_or(BAD)?;
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) { return Err(BAD); }
    match port.parse::<u32>() { Ok(p) if (1024..=65535).contains(&p) && p.to_string() == port => Ok(()), _ => Err(BAD) }
}
pub fn valid_client(s: &str) -> Result<(), &'static str> { if CLIENTS.contains(&s) { Ok(()) } else { Err("That client is not allowed to sign in this way.") } }
/// 1-60 plain characters (letters, digits, space, - _ . ( )), not only spaces; the name as it will be shown (trimmed).
pub fn clean_device(s: &str) -> Result<String, &'static str> {
    const BAD: &str = "The device name must be 1 to 60 plain characters (letters, digits, space and - _ . ( )).";
    if s.is_empty() || s.chars().count() > 60 || !s.chars().all(|c| c.is_ascii_alphanumeric() || " -_.()".contains(c)) { return Err(BAD); }
    let t = s.trim();
    if t.is_empty() { Err(BAD) } else { Ok(t.to_string()) }
}
fn urlsafe(s: &str, extra: &str) -> bool { s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c)) }
/// 8-128 URL-safe characters.
pub fn valid_state(s: &str) -> Result<(), &'static str> { if (8..=128).contains(&s.len()) && urlsafe(s, "-._~") { Ok(()) } else { Err("The state must be 8 to 128 URL-safe characters.") } }
/// base64url of a sha256: exactly 43 characters.
pub fn valid_challenge(s: &str) -> Result<(), &'static str> { if s.len() == 43 && urlsafe(s, "-_") { Ok(()) } else { Err("The challenge must be 43 base64url characters.") } }
/// RFC 7636: 43-128 unreserved characters.
pub fn valid_verifier(s: &str) -> Result<(), &'static str> { if (43..=128).contains(&s.len()) && urlsafe(s, "-._~") { Ok(()) } else { Err("The verifier must be 43 to 128 URL-safe characters.") } }
/// `base64url(sha256(verifier))` without padding.
pub fn pkce_challenge(verifier: &str) -> String { use base64::Engine; base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pgm_core_internal::crypto::sha256(verifier.as_bytes())) }
fn valid_code(s: &str) -> bool { s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }

type Fail = (u16, String);
fn bad(e: &str) -> Fail { (400, e.to_string()) }

/// The request's parts, validated: (device as shown).
fn parts(client: &str, device: &str, redirect: &str, state: &str, challenge: &str) -> Result<String, Fail> {
    valid_client(client).map_err(bad)?;
    let d = clean_device(device).map_err(bad)?;
    valid_redirect(redirect).map_err(bad)?;
    valid_state(state).map_err(bad)?;
    valid_challenge(challenge).map_err(bad)?;
    Ok(d)
}

impl Devices {
    /// Config `appSignin.codeSeconds`: how long a code lives (1 to 600; 120 by default).
    pub fn set_app_code_seconds(&self, s: u64) { self.app.seconds.store(s.clamp(1, 600), Ordering::Relaxed); }

    fn app_rate(&self, bucket: &str, ip: &str, max: usize) -> Result<(), Fail> {
        let now = now_ms();
        let mut s = lk(&self.starts);
        let v = s.entry(format!("{bucket}:{ip}")).or_default();
        v.retain(|t| *t + 60_000 > now);
        if v.len() >= max { return Err((429, "Too many requests from here. Try again in a minute.".into())); }
        v.push(now);
        if s.len() > 500 { s.retain(|_, v| v.iter().any(|t| *t + 60_000 > now)); }
        Ok(())
    }
    fn app_log(action: &str, actor: &str, client: &str, device: &str, reason: &str, ip: &str) {
        // only what passed the validators goes in: never the code, the verifier or the token
        let c = if valid_client(client).is_ok() { client } else { "" };
        let d = clean_device(device).unwrap_or_default();
        let mut detail = json!({ "client": c, "device": d });
        if !reason.is_empty() { detail["reason"] = json!(reason); }
        audit::record(actor, action, c, detail, ip);
    }

    /// GET /api/app-signin/check: is this a request the page may show (and navigate back for)? `{ok, device, client}`.
    pub fn app_check(&self, ip: &str, client: &str, device: &str, redirect: &str, state: &str, challenge: &str) -> Result<Value, Fail> {
        self.app_rate("app-check", ip, CHECKS_PER_MINUTE)?;
        let d = parts(client, device, redirect, state, challenge)?;
        Ok(json!({ "ok": true, "device": d, "client": client }))
    }

    /// POST /api/app-signin/authorize (the person is signed in): a one-time code; the answer is where to send the browser.
    pub fn app_authorize(&self, user: &str, ip: &str, client: &str, device: &str, redirect: &str, state: &str, challenge: &str) -> Result<Value, Fail> {
        let refuse = |f: Fail, why: &str| { Self::app_log("app-signin.refused", user, client, device, why, ip); f };
        self.app_rate("app-auth", ip, AUTHORIZES_PER_MINUTE).map_err(|f| refuse(f, "rate"))?;
        let d = parts(client, device, redirect, state, challenge).map_err(|f| refuse(f, "invalid"))?;
        let now = now_ms();
        let code = pgm_core_internal::crypto::random_hex(32);
        {
            let mut c = lk(&self.app.codes);
            c.retain(|_, r| r.expires + KEEP_AFTER_MS > now);
            if c.values().filter(|r| matches!(r.state, Life::Open) && r.expires > now).count() >= MAX_OPEN { drop(c); return Err(refuse((429, "Too many sign-in requests are open. Try again in a few minutes.".into()), "full")); }
            if c.len() >= MAX_KEPT { c.retain(|_, r| matches!(r.state, Life::Open) && r.expires > now); }
            let secs = self.app.seconds.load(Ordering::Relaxed);
            c.insert(key(&code), Code { user: user.to_string(), challenge: challenge.to_string(), redirect: redirect.to_string(), client: client.to_string(), device: d.clone(), expires: now + secs * 1000, state: Life::Open });
        }
        Self::app_log("app-signin.authorize", user, client, &d, "", ip);
        Ok(json!({ "redirectTo": format!("{redirect}?code={code}&state={state}") }))
    }

    /// POST /api/app-signin/exchange (no sign-in): the code and its verifier for a per-device token `{token, username, deviceId}`.
    pub fn app_exchange(&self, ip: &str, code: &str, verifier: &str, redirect: &str, client: &str, device: &str) -> Result<Value, Fail> {
        let none = |f: Fail, who: &str, why: &str| { Self::app_log("app-signin.refused", who, client, device, why, ip); f };
        self.app_rate("app-exchange", ip, EXCHANGES_PER_MINUTE).map_err(|f| none(f, "", "rate"))?;
        let d = match (valid_code(code), valid_verifier(verifier), valid_redirect(redirect), valid_client(client), clean_device(device)) {
            (true, Ok(()), Ok(()), Ok(()), Ok(d)) => d,
            _ => return Err(none(bad("That request is not valid."), "", "invalid")),
        };
        let gone = "That code is unknown, expired or already used.";
        let now = now_ms();
        let k = key(code);
        let mut c = lk(&self.app.codes);
        c.retain(|_, r| r.expires + KEEP_AFTER_MS > now);
        let Some(r) = c.get_mut(&k) else { drop(c); return Err(none(bad(gone), "", "unknown")) };
        let user = r.user.clone();
        match &r.state {
            Life::Used { id } => {
                // presented twice: whatever the first exchange made is revoked
                let id = id.clone();
                r.state = Life::Burnt;
                drop(c);
                self.revoke(&user, &id);
                return Err(none(bad(gone), &user, "reused"));
            }
            Life::Taking => { r.state = Life::Burnt; drop(c); return Err(none(bad(gone), &user, "reused")); }
            Life::Burnt => { drop(c); return Err(none(bad(gone), &user, "used")); }
            Life::Open => {}
        }
        if r.expires < now { r.state = Life::Burnt; drop(c); return Err(none(bad("That code has expired: start again on the device."), &user, "expired")); }
        // every binding compared, no early exit; a wrong one burns the code
        let ok = same(&pkce_challenge(verifier), &r.challenge) & same(redirect, &r.redirect) & same(client, &r.client) & same(&d, &r.device);
        if !ok { r.state = Life::Burnt; drop(c); return Err(none(bad("That code does not match this request."), &user, "mismatch")); }
        r.state = Life::Taking;
        drop(c);
        let (token, id) = self.issue(&user, client, &d);
        let mut c = lk(&self.app.codes);
        match c.get_mut(&k) {
            Some(r) if matches!(r.state, Life::Taking) => { r.state = Life::Used { id: id.clone() }; }
            _ => { drop(c); self.revoke(&user, &id); return Err(none(bad(gone), &user, "reused")); } // presented again while the token was being made
        }
        drop(c);
        Self::app_log("app-signin.exchange", &user, client, &d, "", ip);
        Ok(json!({ "token": token, "username": user, "deviceId": id }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_are_loopback_with_a_real_port_and_nothing_more() {
        let ok = ["http://127.0.0.1:1024/cb", "http://127.0.0.1:54321/cb", "http://localhost:65535/cb", "http://localhost:8080/cb"];
        for r in ok { assert!(valid_redirect(r).is_ok(), "{r}"); }
        let no = ["", "http://127.0.0.1/cb", "http://127.0.0.1:1023/cb", "http://127.0.0.1:65536/cb", "http://127.0.0.1:0/cb", "http://127.0.0.1:01024/cb", "http://127.0.0.1:+1024/cb", "http://127.0.0.1:99999999/cb",
            "https://127.0.0.1:5000/cb", "http://127.0.0.1:5000/cb/", "http://127.0.0.1:5000/cb?x=1", "http://127.0.0.1:5000/cb#f", "http://127.0.0.1:5000/other", "http://127.0.0.1:5000",
            "http://user@127.0.0.1:5000/cb", "http://user:pw@localhost:5000/cb", "http://127.0.0.1.evil.com:5000/cb", "http://localhost.evil.com:5000/cb", "http://evil.com:5000/cb", "http://[::1]:5000/cb",
            "http://0.0.0.0:5000/cb", "http://127.0.0.2:5000/cb", "HTTP://127.0.0.1:5000/cb", "http://127.0.0.1:5000/cb\n", " http://127.0.0.1:5000/cb", "http://localhost:5000/cb@evil.com", "//127.0.0.1:5000/cb", "javascript:alert(1)"];
        for r in no { assert!(valid_redirect(r).is_err(), "{r:?}"); }
    }

    #[test]
    fn clients_devices_states_challenges_verifiers() {
        assert!(valid_client("jcodepgm").is_ok() && valid_client("jcodepgm-desktop").is_ok());
        for c in ["", "jcode", "JCODEPGM", "vr-machine", "jcodepgm ", "jcodepgm-desktop2"] { assert!(valid_client(c).is_err(), "{c}"); }
        assert_eq!(clean_device("My Laptop (work)_1.2-b").unwrap(), "My Laptop (work)_1.2-b");
        assert_eq!(clean_device(&"a".repeat(60)).unwrap().len(), 60);
        assert_eq!(clean_device(" pad ").unwrap(), "pad");
        for d in ["", "   ", &"a".repeat(61), "bad/slash", "<b>x</b>", "emoji \u{1F600}", "tab\there", "new\nline", "quote\"", "caf\u{e9}", "a;b", "a&b=c"] { assert!(clean_device(d).is_err(), "{d:?}"); }
        assert!(valid_state("abcdefgh").is_ok() && valid_state(&"A-z_0.~".repeat(19)[..128]).is_ok());
        for s in ["", "abcdefg", &"a".repeat(129), "abcd efgh", "abcdefg&", "abcdefg=", "abcdefg%20", "abcdefg/", "abcdefg?"] { assert!(valid_state(s).is_err(), "{s:?}"); }
        let c = pkce_challenge(&"v".repeat(43));
        assert_eq!(c.len(), 43);
        assert!(valid_challenge(&c).is_ok());
        for s in ["", &"a".repeat(42), &"a".repeat(44), &format!("{}=", "a".repeat(42)), &format!("{}+", "a".repeat(42)), &format!("{}.", "a".repeat(42))] { assert!(valid_challenge(s).is_err(), "{s:?}"); }
        assert!(valid_verifier(&"a".repeat(43)).is_ok() && valid_verifier(&"a".repeat(128)).is_ok() && valid_verifier(&format!("{}-._~", "a".repeat(40))).is_ok());
        for s in ["", &"a".repeat(42), &"a".repeat(129), &format!("{}+", "a".repeat(42)), &format!("{} ", "a".repeat(42))] { assert!(valid_verifier(s).is_err(), "{s:?}"); }
    }

    #[test]
    fn pkce_matches_rfc_7636_appendix_b() {
        assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    fn dev() -> Devices {
        let d = std::env::temp_dir().join(format!("vr-app-{}", pgm_core_internal::crypto::random_hex(4)));
        std::fs::create_dir_all(&d).unwrap();
        Devices::new(&d)
    }
    const RD: &str = "http://127.0.0.1:50123/cb";
    const ST: &str = "state-0123456789";
    fn code_of(v: &Value) -> String { v["redirectTo"].as_str().unwrap().split("code=").nth(1).unwrap().split('&').next().unwrap().to_string() }

    #[test]
    fn the_flow_single_use_and_replay_revokes() {
        let d = dev();
        let verifier = "v".repeat(50);
        let ch = pkce_challenge(&verifier);
        let a = d.app_authorize("sam", "ip1", "jcodepgm", "Sam Laptop", RD, ST, &ch).unwrap();
        assert!(a["redirectTo"].as_str().unwrap().starts_with(&format!("{RD}?code=")) && a["redirectTo"].as_str().unwrap().ends_with(&format!("&state={ST}")));
        let code = code_of(&a);
        let x = d.app_exchange("ip1", &code, &verifier, RD, "jcodepgm", "Sam Laptop").unwrap();
        let tok = x["token"].as_str().unwrap().to_string();
        assert_eq!(x["username"], "sam");
        assert_eq!(d.user_of(&tok).as_deref(), Some("sam"));
        let l = d.list("sam", None);
        assert!(l.len() == 1 && l[0]["name"] == "Sam Laptop" && l[0]["client"] == "jcodepgm" && l[0]["id"] == x["deviceId"]);
        // a second presentation: refused, and the token the first made is revoked
        assert_eq!(d.app_exchange("ip1", &code, &verifier, RD, "jcodepgm", "Sam Laptop").unwrap_err().0, 400);
        assert!(d.user_of(&tok).is_none() && d.list("sam", None).is_empty());
    }

    #[test]
    fn wrong_bindings_burn_the_code() {
        let d = dev();
        let verifier = "v".repeat(50);
        let ch = pkce_challenge(&verifier);
        let cases: [(&str, &str, &str, &str); 4] = [(&"w".repeat(50), RD, "jcodepgm", "Dev"), (&verifier, "http://localhost:50123/cb", "jcodepgm", "Dev"), (&verifier, RD, "jcodepgm-desktop", "Dev"), (&verifier, RD, "jcodepgm", "Other")];
        for (v, r, c, dn) in cases {
            let code = code_of(&d.app_authorize("sam", "ip", "jcodepgm", "Dev", RD, ST, &ch).unwrap());
            assert_eq!(d.app_exchange("ip", &code, v, r, c, dn).unwrap_err().0, 400, "{v} {r} {c} {dn}");
            // the right one afterwards is no good either
            assert_eq!(d.app_exchange("ip", &code, &verifier, RD, "jcodepgm", "Dev").unwrap_err().0, 400);
        }
        assert!(d.list("sam", None).is_empty());
        // unknown, malformed
        assert_eq!(d.app_exchange("ip", &"0".repeat(64), &verifier, RD, "jcodepgm", "Dev").unwrap_err().0, 400);
        assert_eq!(d.app_exchange("ip", "short", &verifier, RD, "jcodepgm", "Dev").unwrap_err().0, 400);
    }

    #[test]
    fn expiry_and_refused_requests_and_the_rate() {
        let d = dev();
        d.set_app_code_seconds(1);
        let verifier = "v".repeat(50);
        let ch = pkce_challenge(&verifier);
        let code = code_of(&d.app_authorize("sam", "ip", "jcodepgm", "Dev", RD, ST, &ch).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert_eq!(d.app_exchange("ip", &code, &verifier, RD, "jcodepgm", "Dev").unwrap_err().0, 400);
        for (c, dn, r, s, h) in [("nope", "Dev", RD, ST, &ch[..]), ("jcodepgm", "", RD, ST, &ch[..]), ("jcodepgm", "Dev", "http://evil.com:5000/cb", ST, &ch[..]), ("jcodepgm", "Dev", RD, "short", &ch[..]), ("jcodepgm", "Dev", RD, ST, "short")] {
            assert_eq!(d.app_authorize("sam", "ip", c, dn, r, s, h).unwrap_err().0, 400);
            assert_eq!(d.app_check("ip", c, dn, r, s, h).unwrap_err().0, 400);
        }
        assert_eq!(d.app_check("ip", "jcodepgm-desktop", "My PC", RD, ST, &ch).unwrap()["device"], "My PC");
        let mut last = 0;
        for _ in 0..12 { last = match d.app_exchange("rate-ip", &"0".repeat(64), &verifier, RD, "jcodepgm", "Dev") { Err((s, _)) => s, Ok(_) => 200 }; }
        assert_eq!(last, 429);
        assert_eq!(d.app_exchange("another-ip", &"0".repeat(64), &verifier, RD, "jcodepgm", "Dev").unwrap_err().0, 400);
    }

    #[test]
    fn code_shapes() {
        assert!(valid_code(&"ab01".repeat(16)));
        for c in ["", &"ab01".repeat(15), &"AB01".repeat(16), &"zz".repeat(32), &format!("{}0", "ab01".repeat(16))] { assert!(!valid_code(c), "{c:?}"); }
    }
}
