//! What everyone sees about a person and how the app behaves for them (from VibeRoller's hub profile.rs, 5.3.0): the
//! profile (bio, pronouns, status, location, links, banner colour, time zone), the nickname + colour, the app settings
//! (prefs: sounds, speech, calls, theme …), and the profile picture (`<avatars>/<username>.<webp|png|jpg>`). Kept where
//! they always were: the account's state.json "profile" / "prefs". Routes: /api/profile, /api/profile/avatar,
//! /api/profile/avatar/remove, /api/prefs.
use serde_json::{json, Map, Value};
use std::path::Path;

use crate::chat::{public_prefs_of, ChatHost, Resp};
use pgm_core_internal::json::write_json;
use pgm_identity_internal::Account;
use pgm_net_internal::http::{json as resp_json, Req};

const PALETTE: i64 = 16;
fn answer(status: u16, v: Value) -> Resp { resp_json(status, &v) }
fn state_file(a: &Account) -> std::path::PathBuf { a.dir.join("state.json") }
fn read_state(a: &Account) -> Value { std::fs::read_to_string(state_file(a)).ok().and_then(|t| serde_json::from_str(&t).ok()).filter(Value::is_object).unwrap_or(json!({})) }
fn write_state(a: &Account, f: impl FnOnce(&mut Map<String, Value>)) -> Value { let mut v = read_state(a); f(v.as_object_mut().unwrap()); write_json(&state_file(a), &v); v }
fn units(t: &str, max: usize) -> String { crate::guilds::take_units(t, max) }
fn text(v: &Value, max: usize) -> String { let t = match v { Value::String(x) => x.clone(), Value::Null => String::new(), o => o.to_string() }; units(t.chars().filter(|c| { let n = *c as u32; !(n < 0x20 && n != 0x09 && n != 0x0a) && n != 0x7f }).collect::<String>().trim(), max) }
fn one_line(v: &Value, max: usize) -> String { text(v, max).split_whitespace().collect::<Vec<_>>().join(" ") }
fn int_in(v: &Value, lo: i64, hi: i64) -> Option<i64> { v.as_i64().filter(|_| v.is_i64() || v.is_u64()).filter(|n| *n >= lo && *n < hi) }
fn clamp(v: &Value, lo: f64, hi: f64, d: f64) -> f64 { let n = v.as_f64().or_else(|| v.as_str().and_then(|x| x.parse().ok())).filter(|x| *x != 0.0).unwrap_or(d); n.round().clamp(lo, hi) }
fn truthy(v: &Value) -> bool { !(v.is_null() || *v == json!(false) || *v == json!(0) || *v == json!("") || *v == json!("false")) }
/// A time zone name the clock knows ("Europe/Paris"), or None.
pub fn tz_ok(z: &str) -> Option<String> { if z.is_empty() || z.len() > 64 { return None; } z.parse::<chrono_tz::Tz>().ok().map(|_| z.to_string()) }

pub fn clean_profile(p: &Value, prev: &Value) -> Result<Value, String> {
    let mut out = if prev.is_object() { prev.clone() } else { json!({}) };
    if !p.is_object() { return Ok(out); }
    if p.get("bio").is_some() { out["bio"] = json!(regex::Regex::new(r"\n{3,}").unwrap().replace_all(&text(&p["bio"], 300), "\n\n")); }
    if p.get("pronouns").is_some() { out["pronouns"] = json!(one_line(&p["pronouns"], 40)); }
    if p.get("status").is_some() { out["status"] = json!(one_line(&p["status"], 80)); }
    if p.get("location").is_some() { out["location"] = json!(one_line(&p["location"], 60)); }
    if p.get("links").is_some() {
        let list: Vec<String> = match &p["links"] { Value::Array(a) => a.iter().map(|x| match x { Value::String(t) => t.trim().to_string(), o => o.to_string() }).collect(), o => one_line(o, 2000).split(|c: char| c == ',' || c.is_whitespace()).map(|x| x.trim().to_string()).collect() };
        let list: Vec<String> = list.into_iter().filter(|x| !x.is_empty()).collect();
        let re = regex::Regex::new(r"(?i)^https://[^\s/]+\.\S+$").unwrap();
        if let Some(bad) = list.iter().find(|l| !re.is_match(l) || l.len() > 200) { return Err(format!("Links must be https:// addresses (this one is not: {}).", bad.chars().take(60).collect::<String>())); }
        let mut uniq: Vec<String> = vec![]; for l in list { if !uniq.contains(&l) { uniq.push(l); } } uniq.truncate(3);
        out["links"] = json!(uniq);
    }
    if p.get("accent").is_some() { out["accent"] = int_in(&p["accent"], 0, PALETTE).map(|n| json!(n)).unwrap_or(Value::Null); }
    if p.get("timezone").is_some() { let tz = one_line(&p["timezone"], 60); if !tz.is_empty() && tz_ok(&tz).is_none() { return Err(format!("Unknown time zone: {tz}")); } out["timezone"] = json!(tz); }
    Ok(out)
}
pub fn clean_prefs(p: &Value, prev: &Value) -> Value {
    let mut out = if prev.is_object() { prev.clone() } else { json!({}) };
    let has = |k: &str| p.get(k).is_some();
    let b = |v: &Value| truthy(v);
    if has("nick") { let t = match &p["nick"] { Value::String(x) => x.clone(), o => o.to_string() }; out["nick"] = json!(units(t.chars().filter(|c| { let n = *c as u32; n >= 0x20 && n != 0x7f }).collect::<String>().trim(), 32)); }
    if has("color") { out["color"] = int_in(&p["color"], 0, PALETTE).map(|n| json!(n)).unwrap_or(Value::Null); }
    if has("toasts") { out["toasts"] = json!(b(&p["toasts"])); }
    if has("autoScroll") { out["autoScroll"] = json!(p["autoScroll"] == json!(true)); }
    if has("theme") { let t = match &p["theme"] { Value::String(x) => x.clone(), o => o.to_string() }; out["theme"] = json!(if t.len() <= 40 && t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') { t } else { String::new() }); }
    if has("autosave") { out["autosave"] = json!(clamp(&p["autosave"], 0.0, 120.0, 0.0) as i64); }
    if p["sound"].is_object() { let mut s = if out["sound"].is_object() { out["sound"].clone() } else { json!({}) }; let q = &p["sound"]; if q.get("on").is_some() { s["on"] = json!(b(&q["on"])); } for k in ["message", "mention", "reply", "call", "board", "ask"] { if q.get(k).is_some() { s[k] = json!(clamp(&q[k], 0.0, 100.0, 0.0) as i64); } } if let Some(v) = q["set"].as_str().filter(|v| *v == "classic" || *v == "patterns") { s["set"] = json!(v); } out["sound"] = s; }
    if p["tts"].is_object() {
        let mut t = if out["tts"].is_object() { out["tts"].clone() } else { json!({}) }; let q = &p["tts"];
        for k in ["all", "unity", "on", "ask"] { if q.get(k).is_some() { t[k] = json!(b(&q[k])); } }
        if q.get("volume").is_some() { t["volume"] = json!(clamp(&q["volume"], 0.0, 100.0, 0.0) as i64); }
        if q.get("rate").is_some() { t["rate"] = json!(clamp(&q["rate"], 50.0, 200.0, 100.0) as i64); }
        if q.get("voice").is_some() { t["voice"] = json!(units(&match &q["voice"] { Value::String(x) => x.clone(), o => o.to_string() }, 120)); }
        if q.get("scope").is_some() { t["scope"] = json!(if q["scope"] == json!("mine") { "mine" } else { "all" }); }
        out["tts"] = t;
    }
    // when the AI asks (T-248): wait `minutes` without you, then the recommended answers; any activity waits
    if p["ask"].is_object() { let mut a = if out["ask"].is_object() { out["ask"].clone() } else { json!({}) }; let q = &p["ask"]; for k in ["auto", "activity"] { if q.get(k).is_some() { a[k] = json!(b(&q[k])); } } if q.get("minutes").is_some() { a["minutes"] = json!(clamp(&q["minutes"], 1.0, 1440.0, 10.0) as i64); } out["ask"] = a; }
    if p["audio"].is_object() { let mut a =if out["audio"].is_object() { out["audio"].clone() } else { json!({}) }; for k in ["noise", "echo", "agc"] { if p["audio"].get(k).is_some() { a[k] = json!(b(&p["audio"][k])); } } out["audio"] = a; }
    if p["call"].is_object() { let mut c = if out["call"].is_object() { out["call"].clone() } else { json!({}) }; let q = &p["call"]; if q.get("volume").is_some() { c["volume"] = json!(clamp(&q["volume"], 0.0, 100.0, 0.0) as i64); } if q.get("quality").is_some() { c["quality"] = json!(if ["low", "medium", "high"].contains(&q["quality"].as_str().unwrap_or("")) { q["quality"].clone() } else { json!("medium") }); } out["call"] = c; }
    if p["clips"].is_object() { let mut c = if out["clips"].is_object() { out["clips"].clone() } else { json!({}) }; let q = &p["clips"]; if q.get("volume").is_some() { c["volume"] = json!(clamp(&q["volume"], 0.0, 100.0, 0.0) as i64); } if q.get("autoplay").is_some() { c["autoplay"] = json!(b(&q["autoplay"])); } out["clips"] = c; }
    out
}
fn with_user(a: &Account, pp: Value) -> Value { let mut v = json!({ "username": a.username, "display": a.display }); for (k, x) in pp.as_object().cloned().unwrap_or_default() { v[k] = x; } v }

/// The profile and prefs routes; `avatars`: the folder pictures are kept in.
pub fn route(hub: &dyn ChatHost, r: &Req, a: &Account, avatars: &Path) -> Option<Resp> {
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let c = hub.chat();
    let pp = || public_prefs_of(hub, a);
    Some(match r.path.as_str() {
        "/api/profile" if get => answer(200, with_user(a, pp())),
        "/api/profile" if post => {
            let b = r.json(); let st = read_state(a);
            let profile = match clean_profile(&b["profile"], &st["profile"]) { Ok(p) => p, Err(e) => return Some(answer(400, json!({ "error": e }))) };
            let mut q = json!({}); if b.get("nick").is_some() { q["nick"] = b["nick"].clone(); } if b.get("color").is_some() { q["color"] = b["color"].clone(); }
            let prefs = clean_prefs(&q, &st["prefs"]);
            write_state(a, |m| { m.insert("profile".into(), profile); m.insert("prefs".into(), prefs.clone()); });
            c.keys_changed(); // everyone's page reloads the member list
            let mut v = with_user(a, pp()); v["prefs"] = prefs; answer(200, v)
        }
        "/api/profile/avatar" if post => {
            let img = r.json()["image"].as_str().unwrap_or("").to_string();
            let Some(cap) = regex::Regex::new(r"^data:image/(webp|png|jpeg);base64,([A-Za-z0-9+/=]+)$").unwrap().captures(&img) else { return Some(answer(400, json!({ "error": "Send the picture as a WebP, PNG or JPEG image." }))) };
            let buf = pgm_core_internal::unb64(&cap[2]).unwrap_or_default();
            if buf.is_empty() || buf.len() > 300 * 1024 { return Some(answer(400, json!({ "error": "That picture is too large (up to 300 KB after shrinking)." }))); }
            let ext = if buf.len() > 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP" { "webp" } else if buf.len() > 8 && buf[0] == 0x89 && &buf[1..4] == b"PNG" { "png" } else if buf.len() > 3 && buf[0..3] == [0xff, 0xd8, 0xff] { "jpg" } else { return Some(answer(400, json!({ "error": "That is not a WebP, PNG or JPEG picture." }))) };
            let _ = std::fs::create_dir_all(avatars);
            for e in ["webp", "png", "jpg"] { if e != ext { let _ = std::fs::remove_file(avatars.join(format!("{}.{e}", a.username))); } }
            let f = avatars.join(format!("{}.{ext}", a.username)); let tmp = f.with_extension("tmp");
            if std::fs::write(&tmp, &buf).is_err() || std::fs::rename(&tmp, &f).is_err() { return Some(answer(500, json!({ "error": "Could not save the picture." }))); }
            c.keys_changed();
            let p = pp(); answer(200, json!({ "username": a.username, "nick": p["nick"], "color": p["color"], "profile": p["profile"] }))
        }
        "/api/profile/avatar/remove" if post => {
            for e in ["webp", "png", "jpg"] { let _ = std::fs::remove_file(avatars.join(format!("{}.{e}", a.username))); }
            c.keys_changed();
            let p = pp(); answer(200, json!({ "username": a.username, "nick": p["nick"], "color": p["color"], "profile": p["profile"] }))
        }
        "/api/prefs" if get => { let p = read_state(a)["prefs"].clone(); answer(200, json!({ "prefs": if p.is_object() { p } else { json!({}) } })) }
        "/api/prefs" if post => { let st = read_state(a); let prefs = clean_prefs(&r.json(), &st["prefs"]); write_state(a, |m| { m.insert("prefs".into(), prefs.clone()); }); answer(200, json!({ "prefs": prefs })) }
        _ => return None,
    })
}
