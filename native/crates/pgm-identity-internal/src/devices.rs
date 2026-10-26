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
const MAX_MACHINES_PER_USER: usize = 20;
/// A machine is online while its poll is held, and for this long after the last one (presence.md: 70 s).
pub const ONLINE_MS: u64 = 70_000;
const REVOKED_KEEP_MS: u64 = 30 * 24 * 3600 * 1000;
const REVOKED_MAX: usize = 500;
pub const MACHINE_CLIENT: &str = "vr-machine";

#[derive(Clone, PartialEq)]
enum State { Pending, Approved(String), Denied }
struct Pending { user_code: String, client: String, device: String, expires: u64, last_poll: u64, state: State, meta: Value, replace: Option<String> }
/// Called when a linked machine is unlinked or revoked (`user`, machine record `id`): the place where its poll and its
/// in-flight jobs end (the job engine of T-311 plugs in here).
pub type UnlinkHook = Box<dyn Fn(&str, &str) + Send + Sync>;

pub struct Devices {
    file: PathBuf,
    tokens: Mutex<Map<String, Value>>, // sha256(token) → { id, user, name, client, created, lastUsed, lastUsedAt }
    pending: Mutex<HashMap<String, Pending>>, // sha256(deviceCode) → request
    starts: Mutex<HashMap<String, Vec<u64>>>, // address → recent starts
    fails: Mutex<HashMap<String, (u32, u64)>>, // person → wrong user codes, blocked until
    revoked: Mutex<Map<String, Value>>, // sha256(token) of an unlinked machine → { id, user, at }: the machine is told "revoked", not just "unknown"
    polls: Mutex<HashMap<String, u64>>, // machine record id → last poll (memory only; online = within ONLINE_MS)
    hook: Mutex<Option<UnlinkHook>>,
}

fn clean_code(c: &str) -> String { c.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase() }
fn short(s: &str, n: usize) -> String { s.chars().filter(|c| !c.is_control()).take(n).collect::<String>().trim().to_string() }
fn same(a: &str, b: &str) -> bool { a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes())) }
fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }
fn key(t: &str) -> String { pgm_core_internal::crypto::sha256_hex(t.as_bytes()) }

impl Devices {
    pub fn new(root: &Path) -> Devices {
        let file = root.join("device-tokens.json");
        let doc = read_json(&file);
        let tokens = doc.as_ref().and_then(|v| v["devices"].as_object().cloned()).unwrap_or_default();
        let revoked = doc.as_ref().and_then(|v| v["revoked"].as_object().cloned()).unwrap_or_default();
        Devices { file, tokens: Mutex::new(tokens), pending: Mutex::default(), starts: Mutex::default(), fails: Mutex::default(), revoked: Mutex::new(revoked), polls: Mutex::default(), hook: Mutex::new(None) }
    }
    // `devices` is what the first version wrote (an older reader takes it as it is); `revoked` is new (machines' tombstones)
    fn save(&self) { write_json(&self.file, &json!({ "devices": Value::Object(lk(&self.tokens).clone()), "revoked": Value::Object(lk(&self.revoked).clone()) })); }
    pub fn set_unlink_hook(&self, f: UnlinkHook) { *lk(&self.hook) = Some(f); }
    fn fire(&self, user: &str, id: &str) { if let Some(f) = lk(&self.hook).as_ref() { f(user, id); } lk(&self.polls).remove(id); }

    /// A tool asks to sign in: the codes (or why not). `ttl`: seconds it wants the request to live (never more than 10 minutes).
    pub fn start(&self, client: &str, device: &str, ttl: Option<u64>, ip: &str) -> Result<Value, (u16, String)> { self.start_ex(client, device, ttl, ip, &Value::Null) }

    /// `start` with what a machine says about itself (`vr-machine`: `publicKey` (ed25519, 64 hex, needed), `machineId`
    /// (the machine's own stable id), `os`, `app`, `appVersion`, `mode` (persistent | while open), `folder` (the default
    /// scope's one folder)). The answer then also carries `kind` and the `defaultScope` the machine will start with.
    pub fn start_ex(&self, client: &str, device: &str, ttl: Option<u64>, ip: &str, info: &Value) -> Result<Value, (u16, String)> {
        let machine = short(client, 40) == MACHINE_CLIENT;
        let mut meta = json!({ "ip": short(ip, 80) });
        if machine {
            let pk = info["publicKey"].as_str().unwrap_or("").trim().to_ascii_lowercase();
            if let Err(e) = pgm_net_internal::machine::device::parse_public(&pk) { return Err((400, e)); }
            let mode = if info["mode"].as_str() == Some("while open") { "while open" } else { "persistent" };
            let f = |k: &str, n: usize| short(info[k].as_str().unwrap_or(""), n);
            meta = json!({ "ip": short(ip, 80), "publicKey": pk, "machineId": f("machineId", 64), "os": f("os", 40), "app": f("app", 60), "appVersion": f("appVersion", 30), "mode": mode, "folder": f("folder", 300) });
        }
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
        p.insert(key(&code), Pending { user_code: user_code.clone(), client: short(client, 40), device: short(device, 60), expires: now + secs * 1000, last_poll: 0, state: State::Pending, meta: meta.clone(), replace: None });
        let mut out = json!({ "deviceCode": code, "userCode": format!("{}-{}", &user_code[..4], &user_code[4..]), "expiresIn": secs, "interval": INTERVAL_S });
        if machine { out["kind"] = json!("machine"); out["defaultScope"] = default_scope(&meta); }
        Ok(out)
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
                let (client, device, meta, replace) = (r.client.clone(), r.device.clone(), r.meta.clone(), r.replace.clone());
                p.remove(&k);
                drop(p);
                let token = pgm_core_internal::crypto::random_hex(32);
                let id = pgm_core_internal::crypto::random_hex(6);
                if client == MACHINE_CLIENT { return self.link_machine(&user, &token, &id, &device, &meta, replace.as_deref()); }
                {
                    let mut t = lk(&self.tokens);
                    let mine: Vec<String> = t.iter().filter(|(_, v)| v["user"].as_str() == Some(&user) && v["kind"].as_str() != Some("machine")).map(|(k, _)| k.clone()).collect();
                    if mine.len() >= MAX_DEVICES_PER_USER { if let Some(old) = mine.iter().min_by_key(|k| t[*k]["lastUsedAt"].as_u64().unwrap_or(0)).cloned() { t.remove(&old); } }
                    t.insert(key(&token), json!({ "id": id, "user": user, "name": if device.is_empty() { client.clone() } else { device }, "client": client, "created": iso_now(), "createdAt": now, "lastUsed": Value::Null, "lastUsedAt": now }));
                }
                self.save();
                (200, json!({ "token": token, "username": user, "deviceId": id }))
            }
        }
    }

    /// The signed-in person approves or refuses a user code.
    pub fn decide(&self, user: &str, user_code: &str, approve: bool) -> Result<Value, (u16, String)> { self.decide_ex(user, user_code, approve, None) }

    /// What the approval screen shows for a user code, before the person answers: who is asking, a machine's facts,
    /// the default scope it will start with, and which of the person's own records it could replace (3.5).
    pub fn peek(&self, user: &str, user_code: &str) -> Result<Value, (u16, String)> {
        let now = now_ms();
        let want = clean_code(user_code);
        self.check_blocked(user, now)?;
        let mut p = lk(&self.pending);
        p.retain(|_, r| r.expires + 60_000 > now);
        let mut hit = None;
        for (k, r) in p.iter() { if same(&r.user_code, &want) { hit = Some(k.clone()); } }
        let Some(k) = hit.filter(|_| want.len() == 8) else { drop(p); return Err(self.wrong_code(user, now)); };
        let r = &p[&k];
        if r.expires < now { return Err((410, "That code has expired: start again on the device.".into())); }
        if r.state != State::Pending { return Err((409, "That code was already answered.".into())); }
        let mut out = json!({ "client": r.client, "device": r.device, "ip": r.meta["ip"], "expiresIn": (r.expires - now) / 1000 });
        if r.client == MACHINE_CLIENT {
            let m = &r.meta;
            out["kind"] = json!("machine");
            for f in ["os", "app", "appVersion", "mode", "machineId"] { out[f] = m[f].clone(); }
            out["keyFingerprint"] = json!(pgm_net_internal::machine::device::fingerprint(m["publicKey"].as_str().unwrap_or("")));
            out["defaultScope"] = default_scope(m);
            let mid = m["machineId"].as_str().unwrap_or("");
            out["replaceable"] = json!(self.machines_of(user).into_iter().filter(|x| (!mid.is_empty() && x["machineId"].as_str() == Some(mid)) || x["name"].as_str() == Some(r.device.as_str()))
                .map(|x| json!({ "id": x["id"], "name": x["name"], "lastSeen": x["lastSeen"], "sameMachineId": !mid.is_empty() && x["machineId"].as_str() == Some(mid) })).collect::<Vec<_>>());
        }
        Ok(out)
    }
    fn check_blocked(&self, user: &str, now: u64) -> Result<(), (u16, String)> {
        let f = lk(&self.fails);
        if let Some((_, until)) = f.get(user) { if *until > now { return Err((429, "Too many wrong codes. Try again in 15 minutes.".into())); } }
        Ok(())
    }
    fn wrong_code(&self, user: &str, now: u64) -> (u16, String) {
        let mut f = lk(&self.fails);
        let e = f.entry(user.to_string()).or_insert((0, 0));
        e.0 += 1;
        if e.0 >= 8 { *e = (0, now + 15 * 60_000); }
        (404, "No such code. Check it and try again.".into())
    }

    /// `replace`: (a machine) the id of one of the person's own machine records the new link takes over from (3.5).
    pub fn decide_ex(&self, user: &str, user_code: &str, approve: bool, replace: Option<&str>) -> Result<Value, (u16, String)> {
        let now = now_ms();
        let want = clean_code(user_code);
        self.check_blocked(user, now)?;
        let mut p = lk(&self.pending);
        p.retain(|_, r| r.expires + 60_000 > now);
        let mut hit = None;
        for (k, r) in p.iter() { if same(&r.user_code, &want) { hit = Some(k.clone()); } } // every entry compared, no early exit
        let Some(k) = hit.filter(|_| want.len() == 8) else { drop(p); return Err(self.wrong_code(user, now)); };
        let r = p.get_mut(&k).expect("found");
        if r.expires < now { p.remove(&k); return Err((410, "That code has expired: start again on the device.".into())); }
        if r.state != State::Pending { return Err((409, "That code was already answered.".into())); }
        let machine = approve && r.client == MACHINE_CLIENT;
        let replace = replace.filter(|x| !x.is_empty());
        if machine {
            let mine = self.machines_of(user);
            if let Some(rep) = replace { if !mine.iter().any(|m| m["id"].as_str() == Some(rep)) { return Err((400, "That machine is not one of yours to replace.".into())); } }
            else if mine.len() >= MAX_MACHINES_PER_USER { return Err((409, format!("You have {MAX_MACHINES_PER_USER} linked machines already: unlink one first."))); }
        }
        r.replace = if machine { replace.map(String::from) } else { None };
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
    /// Is this the token of a linked machine (T-310, plans/linked-machines.md 4.1)? Such a token reaches only /api/machine/*:
    /// the hub refuses it (403) on every other route, whatever the account would have been allowed.
    pub fn is_machine_token(&self, token: &str) -> bool { lk(&self.tokens).get(&key(token)).map(|r| r["kind"].as_str() == Some("machine")).unwrap_or(false) }
    /// A person's own devices (never the token), `current`: the token this request came with. `kind` is "machine" for a
    /// linked machine (its full card is [`machines_of`](Self::machines_of)), "device" for the rest.
    pub fn list(&self, user: &str, current: Option<&str>) -> Vec<Value> {
        let cur = current.map(key);
        let mut v: Vec<Value> = lk(&self.tokens).iter().filter(|(_, r)| r["user"].as_str() == Some(user)).map(|(k, r)| {
            json!({ "id": r["id"], "name": r["name"], "client": r["client"], "kind": r["kind"].as_str().unwrap_or("device"), "created": r["created"], "lastUsed": r["lastUsed"], "current": cur.as_deref() == Some(k.as_str()) })
        }).collect();
        v.sort_by(|a, b| b["created"].as_str().cmp(&a["created"].as_str()));
        v
    }
    /// Revokes one of that person's devices (a machine: its token is remembered as revoked and the unlink hook runs).
    pub fn revoke(&self, user: &str, id: &str) -> bool {
        let gone = {
            let mut t = lk(&self.tokens);
            let k = t.iter().find(|(_, r)| r["user"].as_str() == Some(user) && r["id"].as_str() == Some(id)).map(|(k, _)| k.clone());
            k.and_then(|k| t.remove(&k).map(|r| (k, r)))
        };
        let Some((k, r)) = gone else { return false };
        if r["kind"].as_str() == Some("machine") { self.tombstone(&k, user, id); }
        self.save();
        if r["kind"].as_str() == Some("machine") { self.fire(user, id); }
        true
    }
    /// All of a person's devices go (a new password, an admin's sign-out), machines included.
    pub fn revoke_all(&self, user: &str) { self.revoke_all_ex(user, false) }
    /// Same, but a person who ticked "keep my machines linked" keeps their machines (4: Sponge's answer).
    pub fn revoke_all_ex(&self, user: &str, keep_machines: bool) {
        let gone: Vec<(String, Value)> = {
            let mut t = lk(&self.tokens);
            let ks: Vec<String> = t.iter().filter(|(_, r)| r["user"].as_str() == Some(user) && !(keep_machines && r["kind"].as_str() == Some("machine"))).map(|(k, _)| k.clone()).collect();
            ks.into_iter().filter_map(|k| t.remove(&k).map(|r| (k, r))).collect()
        };
        if gone.is_empty() { return; }
        for (k, r) in &gone { if r["kind"].as_str() == Some("machine") { self.tombstone(k, user, r["id"].as_str().unwrap_or("")); } }
        self.save();
        for (_, r) in &gone { if r["kind"].as_str() == Some("machine") { self.fire(user, r["id"].as_str().unwrap_or("")); } }
    }
    fn tombstone(&self, k: &str, user: &str, id: &str) {
        let now = now_ms();
        let mut r = lk(&self.revoked);
        r.retain(|_, v| v["at"].as_u64().unwrap_or(0) + REVOKED_KEEP_MS > now);
        r.insert(k.to_string(), json!({ "id": id, "user": user, "at": now }));
        while r.len() > REVOKED_MAX { let Some(old) = r.iter().min_by_key(|(_, v)| v["at"].as_u64().unwrap_or(0)).map(|(k, _)| k.clone()) else { break }; r.remove(&old); }
    }

    // ----- linked machines (T-309): a machine record is a device token with `kind: "machine"`, in the same file -----

    fn link_machine(&self, user: &str, token: &str, id: &str, device: &str, meta: &Value, replace: Option<&str>) -> (u16, Value) {
        let now = now_ms();
        let mut name = if device.is_empty() { MACHINE_CLIENT.to_string() } else { device.to_string() };
        let mut first = now;
        let mut replaced = vec![];
        let old = {
            let mut t = lk(&self.tokens);
            let k = replace.and_then(|rep| t.iter().find(|(_, r)| r["user"].as_str() == Some(user) && r["kind"].as_str() == Some("machine") && r["id"].as_str() == Some(rep)).map(|(k, _)| k.clone()));
            let old = k.and_then(|k| t.remove(&k).map(|r| (k, r)));
            if old.is_none() && t.values().filter(|r| r["user"].as_str() == Some(user) && r["kind"].as_str() == Some("machine")).count() >= MAX_MACHINES_PER_USER { return (409, json!({ "error": "Too many linked machines." })); }
            if let Some((_, r)) = &old {
                name = r["name"].as_str().unwrap_or(&name).to_string();
                first = r["linkedAt"].as_u64().unwrap_or(first);
                replaced = r["replaced"].as_array().cloned().unwrap_or_default();
                replaced.push(json!({ "id": r["id"], "name": r["name"], "at": iso_now(), "lastSeen": r["lastSeen"] }));
                replaced.truncate(10);
            }
            // the new link always starts at the default scope: nothing of the old scope carries over (3.5)
            t.insert(key(token), json!({ "id": id, "user": user, "name": name, "client": MACHINE_CLIENT, "kind": "machine", "created": iso_now(), "createdAt": now, "lastUsed": Value::Null, "lastUsedAt": now,
                "linkedAt": first, "machineId": meta["machineId"], "publicKey": meta["publicKey"], "os": meta["os"], "app": meta["app"], "appVersion": meta["appVersion"], "mode": meta["mode"],
                "scopeSummary": default_scope(meta)["summary"], "scopeIsDefault": true, "paused": false, "lastSeenAt": 0, "replaced": replaced }));
            old
        };
        if let Some((k, r)) = &old { self.tombstone(k, user, r["id"].as_str().unwrap_or("")); }
        self.save();
        if let Some((_, r)) = &old { self.fire(user, r["id"].as_str().unwrap_or("")); }
        (200, json!({ "token": token, "username": user, "deviceId": id, "kind": "machine", "machineId": id, "name": name, "defaultScope": default_scope(meta), "replaced": old.map(|(_, r)| r["id"].clone()) }))
    }

    fn machine_card(&self, r: &Value) -> Value {
        let id = r["id"].as_str().unwrap_or("");
        let polled = lk(&self.polls).get(id).copied().unwrap_or(0);
        let seen = polled.max(r["lastSeenAt"].as_u64().unwrap_or(0));
        let online = polled > 0 && polled + ONLINE_MS > now_ms();
        json!({ "id": id, "kind": "machine", "name": r["name"], "os": r["os"], "app": r["app"], "appVersion": r["appVersion"], "mode": r["mode"], "machineId": r["machineId"],
            "scope": r["scopeSummary"], "scopeIsDefault": r["scopeIsDefault"], "paused": r["paused"], "online": online, "lastSeen": if seen > 0 { json!(pgm_core_internal::time::iso_at(seen as i64)) } else { Value::Null },
            "linked": pgm_core_internal::time::iso_at(r["linkedAt"].as_u64().unwrap_or(0) as i64), "replaced": r["replaced"] })
    }
    /// One person's own machines (never the token or the key), newest link first.
    pub fn machines_of(&self, user: &str) -> Vec<Value> {
        let recs: Vec<Value> = lk(&self.tokens).values().filter(|r| r["user"].as_str() == Some(user) && r["kind"].as_str() == Some("machine")).cloned().collect();
        let mut v: Vec<Value> = recs.iter().map(|r| self.machine_card(r)).collect();
        v.sort_by(|a, b| b["linked"].as_str().cmp(&a["linked"].as_str()));
        v
    }
    /// The admin's view (Sponge, 5): per person only how many machines and how many are online. No names, no scopes.
    pub fn machine_counts(&self) -> Vec<Value> {
        let mut per: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
        let recs: Vec<Value> = lk(&self.tokens).values().filter(|r| r["kind"].as_str() == Some("machine")).cloned().collect();
        for r in &recs {
            let e = per.entry(r["user"].as_str().unwrap_or("").to_string()).or_default();
            e.0 += 1;
            if self.machine_card(r)["online"].as_bool() == Some(true) { e.1 += 1; }
        }
        per.into_iter().map(|(u, (n, on))| json!({ "user": u, "machines": n, "online": on })).collect()
    }
    /// Rename and / or pause one of the person's machines. False: not theirs.
    pub fn machine_update(&self, user: &str, id: &str, name: Option<&str>, paused: Option<bool>) -> bool {
        let ok = {
            let mut t = lk(&self.tokens);
            match t.values_mut().find(|r| r["user"].as_str() == Some(user) && r["kind"].as_str() == Some("machine") && r["id"].as_str() == Some(id)) {
                Some(r) => { if let Some(n) = name.map(|n| short(n, 60)).filter(|n| !n.is_empty()) { r["name"] = json!(n); } if let Some(p) = paused { r["paused"] = json!(p); } true }
                None => false,
            }
        };
        if ok { self.save(); }
        ok
    }
    /// The machine's own call with its token (a poll): who and which record, or why not. A held poll calls this about
    /// once a second: it is what closes the poll at once after an unlink.
    pub fn machine_seen(&self, token: &str, held: bool) -> MachineSeen {
        let k = key(token);
        let now = now_ms();
        let mut save = false;
        let found = {
            let mut t = lk(&self.tokens);
            t.get_mut(&k).filter(|r| r["kind"].as_str() == Some("machine")).map(|r| {
                if now > r["lastSeenAt"].as_u64().unwrap_or(0) + 60_000 { r["lastSeenAt"] = json!(now); save = true; }
                (r["user"].as_str().unwrap_or("").to_string(), r["id"].as_str().unwrap_or("").to_string(), r["paused"].as_bool() == Some(true))
            })
        };
        match found {
            Some((user, id, paused)) => { if held { lk(&self.polls).insert(id.clone(), now); } if save { self.save(); } MachineSeen::Linked { user, id, paused } }
            None if lk(&self.revoked).contains_key(&k) => MachineSeen::Revoked,
            None => MachineSeen::Unknown,
        }
    }
    /// The machine tells the hub its present scope in words and mode (it owns the scope; the hub only shows it).
    pub fn machine_report(&self, token: &str, scope: Option<&str>, mode: Option<&str>) {
        let ok = {
            let mut t = lk(&self.tokens);
            match t.get_mut(&key(token)).filter(|r| r["kind"].as_str() == Some("machine")) {
                Some(r) => {
                    if let Some(s) = scope.map(|s| short(s, 400)).filter(|s| !s.is_empty()) { if r["scopeSummary"].as_str() != Some(&s) { r["scopeSummary"] = json!(s); r["scopeIsDefault"] = json!(false); } }
                    if let Some(m) = mode.filter(|m| matches!(*m, "persistent" | "while open")) { r["mode"] = json!(m); }
                    true
                }
                None => false,
            }
        };
        if ok { self.save(); }
    }
    /// The hub's check of a signed reply: the machine's token names the record; the signature must be by the key
    /// recorded at approval, over (record id, request id, body). Returns the owner and the record id.
    pub fn verify_machine_reply(&self, token: &str, request_id: &str, body: &[u8], sig: &str) -> Result<(String, String), (u16, String)> {
        let rec = lk(&self.tokens).get(&key(token)).filter(|r| r["kind"].as_str() == Some("machine")).cloned();
        let Some(r) = rec else { return Err(if lk(&self.revoked).contains_key(&key(token)) { (401, "revoked".into()) } else { (401, "Not signed in.".into()) }) };
        let (id, pk) = (r["id"].as_str().unwrap_or(""), r["publicKey"].as_str().unwrap_or(""));
        if pgm_net_internal::machine::device::verify_reply(pk, id, request_id, body, sig) { Ok((r["user"].as_str().unwrap_or("").to_string(), id.to_string())) } else { Err((403, "The reply is not signed by this machine's key.".into())) }
    }
}

/// What the hub makes of a machine's token.
#[derive(Debug, PartialEq)]
pub enum MachineSeen { Linked { user: String, id: String, paused: bool }, Revoked, Unknown }

/// The default scope a new link starts with (3.1, 13.2): one folder, read-write, shell confined to it. The folder is
/// what the machine named at `start` (it picks it on the machine).
fn default_scope(meta: &Value) -> Value {
    let folder = meta["folder"].as_str().unwrap_or("");
    let summary = if folder.is_empty() { "one folder (picked on the machine), read-write; shell (advisory)".to_string() } else { pgm_net_internal::machine::Scope::default_folder(folder).summary((now_ms() / 1000) as i64) };
    json!({ "preset": "folder", "folder": folder, "access": "rw", "summary": summary })
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
    fn machine_link_bind_revoke_relink() {
        use pgm_net_internal::machine::device::DeviceKey;
        let d = Devices::new(&root());
        let k = DeviceKey::generate();
        assert!(d.start_ex(MACHINE_CLIENT, "pc", None, "x", &json!({ "publicKey": "nope" })).is_err(), "a machine without a valid key cannot start");
        let start = |dev: &str, mid: &str| d.start_ex(MACHINE_CLIENT, dev, None, "1.2.3.4", &json!({ "publicKey": k.public_hex(), "machineId": mid, "os": "linux", "app": "vr-host", "folder": "/work/proj" })).unwrap();
        let s = start("pc", "m-1");
        let (code, uc) = (s["deviceCode"].as_str().unwrap().to_string(), s["userCode"].as_str().unwrap().to_string());
        let peek = d.peek("a", &uc).unwrap();
        assert_eq!(peek["kind"], "machine");
        assert!(peek["replaceable"].as_array().unwrap().is_empty() && peek["keyFingerprint"].as_str().unwrap().len() == 19);
        d.decide("a", &uc, true).unwrap();
        let (st, v) = d.poll(&code);
        assert_eq!(st, 200);
        let (tok, id) = (v["token"].as_str().unwrap().to_string(), v["deviceId"].as_str().unwrap().to_string());
        assert_eq!(d.user_of(&tok).as_deref(), Some("a"));
        assert!(d.machines_of("b").is_empty() && d.machines_of("a").len() == 1);
        assert_eq!(d.machine_counts(), vec![json!({ "user": "a", "machines": 1, "online": 0 })]);
        assert_eq!(d.machine_seen(&tok, true), MachineSeen::Linked { user: "a".into(), id: id.clone(), paused: false });
        assert_eq!(d.machine_counts()[0]["online"], 1);
        // a signed reply
        let sig = k.sign_reply(&id, "r1", b"body");
        assert_eq!(d.verify_machine_reply(&tok, "r1", b"body", &sig).unwrap(), ("a".to_string(), id.clone()));
        assert_eq!(d.verify_machine_reply(&tok, "r1", b"other", &sig).unwrap_err().0, 403);
        assert_eq!(d.verify_machine_reply(&tok, "r1", b"body", &DeviceKey::generate().sign_reply(&id, "r1", b"body")).unwrap_err().0, 403);
        // another account cannot rename / unlink it
        assert!(!d.machine_update("b", &id, Some("x"), None) && d.machine_update("a", &id, Some("Desk"), Some(true)));
        assert_eq!(d.machines_of("a")[0]["name"], "Desk");
        // re-link after a wiped store: offered, replaces, starts at the default scope, old token dies
        d.machine_report(&tok, Some("folders: /everything (read-write)"), None);
        assert_eq!(d.machines_of("a")[0]["scopeIsDefault"], false);
        let s2 = start("pc", "m-1");
        let uc2 = s2["userCode"].as_str().unwrap().to_string();
        let rep = d.peek("a", &uc2).unwrap()["replaceable"].clone();
        assert_eq!(rep[0]["id"], json!(id) ); assert_eq!(rep[0]["sameMachineId"], true);
        assert_eq!(d.decide_ex("a", &uc2, true, Some("nosuch")).unwrap_err().0, 400);
        d.decide_ex("a", &uc2, true, Some(&id)).unwrap();
        let (_, v2) = d.poll(s2["deviceCode"].as_str().unwrap());
        let tok2 = v2["token"].as_str().unwrap().to_string();
        assert_eq!(d.machine_seen(&tok, false), MachineSeen::Revoked);
        assert!(d.user_of(&tok).is_none());
        let ms = d.machines_of("a");
        assert_eq!(ms.len(), 1);
        assert!(ms[0]["scope"].as_str().unwrap().starts_with("one folder: /work/proj") && ms[0]["scopeIsDefault"] == true && ms[0]["name"] == "Desk" && ms[0]["paused"] == false);
        // unlink: hook runs, token is revoked at once
        let fired = std::sync::Arc::new(Mutex::new(vec![]));
        let f2 = fired.clone();
        d.set_unlink_hook(Box::new(move |u, i| f2.lock().unwrap().push(format!("{u}/{i}"))));
        let id2 = ms[0]["id"].as_str().unwrap().to_string();
        assert!(!d.revoke("b", &id2) && d.revoke("a", &id2));
        assert_eq!(fired.lock().unwrap().clone(), vec![format!("a/{id2}")]);
        assert_eq!(d.machine_seen(&tok2, true), MachineSeen::Revoked);
        assert_eq!(d.machine_seen("never-issued", true), MachineSeen::Unknown);
        assert!(d.user_of(&tok2).is_none());
    }

    #[test]
    fn revoke_all_keeps_machines_only_when_asked() {
        use pgm_net_internal::machine::device::DeviceKey;
        let d = Devices::new(&root());
        let k = DeviceKey::generate();
        let mk = |kind_machine: bool| {
            let s = if kind_machine { d.start_ex(MACHINE_CLIENT, "pc", None, "i", &json!({ "publicKey": k.public_hex() })).unwrap() } else { d.start("jcode", "lap", None, "i").unwrap() };
            d.decide("a", s["userCode"].as_str().unwrap(), true).unwrap();
            d.poll(s["deviceCode"].as_str().unwrap()).1["token"].as_str().unwrap().to_string()
        };
        let (m, t) = (mk(true), mk(false));
        d.revoke_all_ex("a", true);
        assert!(d.user_of(&m).is_some() && d.user_of(&t).is_none());
        let t2 = mk(false);
        d.revoke_all("a");
        assert!(d.user_of(&m).is_none() && d.user_of(&t2).is_none());
        assert_eq!(d.machine_seen(&m, false), MachineSeen::Revoked);
        // survives a restart
        let d2 = Devices::new(d.file.parent().unwrap());
        assert_eq!(d2.machine_seen(&m, false), MachineSeen::Revoked);
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
