//! The Download panel: the native programs (pre-built in native/dist) and nothing else, for people with an account
//! only. Each zip carries this hub's address and VERSION.txt (the hub's version + a fingerprint of its contents).
//!   GET /api/downloads, /api/downloads/<id>.zip[?host=<id>]   (?host= puts that person's own host config in)
//!   GET /api/native?app=vr-desktop&target=, /api/native/file?…&name=   the desktop app's signed release (it updates
//!       itself with the session its page hands it); AI hosts update through their own link (/node/update/bin)
//!   /api/pub/*   gone since 4.0.0 (they were public): 410 with what to do instead
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::downloads_text as t;
use super::{answer, Routed};
use crate::front::{Hub, Req};

pub struct Entry { name: String, data: Vec<u8>, exec: bool, binary: bool }
fn e(name: String, data: impl Into<Vec<u8>>) -> Entry { Entry { name, data: data.into(), exec: false, binary: false } }
const TARGETS: [(&str, &str); 2] = [("Windows", "x86_64-pc-windows-gnu"), ("Linux", "x86_64-unknown-linux-gnu")];
const IDS: [&str; 4] = ["app-windows", "app-linux", "host-windows", "host-linux"];
fn crlf(s: &str) -> String { s.replace("\r\n", "\n").replace('\n', "\r\n") }

// ---------- a small zip writer (deflate where it helps, one central directory, unix modes for the .sh files) ----------
fn crc32(b: &[u8]) -> u32 { let mut c = 0xffff_ffffu32; for &x in b { c ^= x as u32; for _ in 0..8 { c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 }; } } c ^ 0xffff_ffff }
pub fn zip(entries: &[Entry]) -> Vec<u8> {
    use chrono::{Datelike, Timelike};
    let d = chrono::Local::now();
    let time = ((d.hour() << 11) | (d.minute() << 5) | (d.second() >> 1)) as u16; let date = (((d.year() - 1980) as u32) << 9 | d.month() << 5 | d.day()) as u16;
    let (mut out, mut central, mut offset) = (vec![], vec![], 0u32);
    for en in entries {
        let mut z = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::new(if en.binary { 6 } else { 9 })); let _ = z.write_all(&en.data); let def = z.finish().unwrap_or_default();
        let (method, body): (u16, &[u8]) = if def.len() < en.data.len() { (8, &def) } else { (0, &en.data) };
        let crc = crc32(&en.data); let name = en.name.as_bytes();
        let mut l = vec![]; for v in [0x04034b50u32] { l.extend(v.to_le_bytes()); } for v in [20u16, 0x0800, method, time, date] { l.extend(v.to_le_bytes()); }
        for v in [crc, body.len() as u32, en.data.len() as u32] { l.extend(v.to_le_bytes()); } for v in [name.len() as u16, 0] { l.extend(v.to_le_bytes()); }
        let mut c = vec![]; c.extend(0x02014b50u32.to_le_bytes()); for v in [0x031eu16, 20, 0x0800, method, time, date] { c.extend(v.to_le_bytes()); }
        for v in [crc, body.len() as u32, en.data.len() as u32] { c.extend(v.to_le_bytes()); } for v in [name.len() as u16, 0, 0, 0, 0] { c.extend(v.to_le_bytes()); }
        c.extend((((if en.exec { 0o100755u32 } else { 0o100644 }) << 16)).to_le_bytes()); c.extend(offset.to_le_bytes());
        offset += (l.len() + name.len() + body.len()) as u32;
        out.extend(l); out.extend(name); out.extend(body); central.extend(c); central.extend(name);
    }
    let cd_len = central.len() as u32; out.extend(central);
    out.extend(0x06054b50u32.to_le_bytes()); for v in [0u16, 0, entries.len() as u16, entries.len() as u16] { out.extend(v.to_le_bytes()); }
    out.extend(cd_len.to_le_bytes()); out.extend(offset.to_le_bytes()); out.extend(0u16.to_le_bytes());
    out
}

pub struct Downloads { dist: PathBuf, cache: Mutex<HashMap<String, (String, String, Arc<Vec<u8>>, String)>> } // id → (hub, stamp, zip, version)
struct Pkg { group: &'static str, os: &'static str, title: &'static str, file: &'static str }
fn pkg(id: &str) -> Option<Pkg> {
    Some(match id {
        "app-windows" => Pkg { group: "native", os: "Windows", title: "VibeRoller (native app)", file: "VibeRoller-windows.zip" },
        "app-linux" => Pkg { group: "native", os: "Linux", title: "VibeRoller (native app)", file: "VibeRoller-linux.zip" },
        "host-windows" => Pkg { group: "native-host", os: "Windows", title: "VibeRoller AI Host (native)", file: "VibeRollerAIHost-native-windows.zip" },
        "host-linux" => Pkg { group: "native-host", os: "Linux", title: "VibeRoller AI Host (native)", file: "VibeRollerAIHost-native-linux.zip" },
        _ => return None,
    })
}
impl Downloads {
    pub fn new(root: &std::path::Path) -> Downloads { Downloads { dist: root.join("..").join("native").join("dist"), cache: Mutex::default() } }
    fn target(os: &str) -> &'static str { TARGETS.iter().find(|x| x.0 == os).map(|x| x.1).unwrap_or("") }
    fn bin(&self, os: &str, n: &str) -> PathBuf { self.dist.join(Self::target(os)).join(if os == "Windows" { format!("{n}.exe") } else { n.to_string() }) }
    fn available(&self, id: &str) -> bool { match pkg(id) { Some(p) => self.bin(p.os, if p.group == "native" { "vr-desktop" } else { "vr-host" }).exists(), None => false } }
    fn bins(&self, folder: &str, os: &str, names: &[&str]) -> Vec<Entry> { names.iter().filter_map(|n| { let f = self.bin(os, n); std::fs::read(&f).ok().map(|data| Entry { name: format!("{folder}/{}", f.file_name().unwrap().to_string_lossy()), data, exec: os != "Windows", binary: true }) }).collect() }
    fn stamp(&self) -> String { TARGETS.iter().map(|(_, t)| std::fs::metadata(self.dist.join(t).join("BUILD.json")).and_then(|m| m.modified()).map(|m| format!("{m:?}")).unwrap_or_default()).collect::<Vec<_>>().join(",") }
    fn build(&self, id: &str, hub: &str, cfg: Option<&Value>) -> Vec<Entry> {
        let cfg_e = |folder: &str| -> Vec<Entry> { cfg.map(|c| e(format!("{folder}/ai-node.json"), serde_json::to_string_pretty(c).unwrap_or_default() + "\n")).into_iter().collect() };
        let nick = cfg.and_then(|c| c["nickname"].as_str());
        match id {
            "app-windows" => { let mut v = self.bins("VibeRoller", "Windows", &["vr-desktop", "vr-host", "vr-hostw"]); let dll = self.dist.join(Self::target("Windows")).join("WebView2Loader.dll"); if let Ok(d) = std::fs::read(dll) { v.push(Entry { name: "VibeRoller/WebView2Loader.dll".into(), data: d, exec: false, binary: true }); } v.push(e("VibeRoller/README.txt".into(), crlf(&t::app_readme(true, hub)))); v }
            "app-linux" => { let mut v = self.bins("VibeRoller", "Linux", &["vr-desktop", "vr-host", "vr-hostw"]); v.push(e("VibeRoller/README.txt".into(), t::app_readme(false, hub))); v }
            "host-windows" => { let mut v = self.bins("VibeRollerAIHost", "Windows", &["vr-host", "vr-hostw"]); v.extend(cfg_e("VibeRollerAIHost")); v.push(e("VibeRollerAIHost/README.txt".into(), crlf(&t::host_readme(true, hub, nick)))); v }
            _ => { let mut v = self.bins("VibeRollerAIHost", "Linux", &["vr-host", "vr-hostw"]); v.extend(cfg_e("VibeRollerAIHost")); v.push(e("VibeRollerAIHost/README.txt".into(), t::host_readme(false, hub, nick))); v } // host-linux
        }
    }
    /// The package as a zip and its version (hub version + a fingerprint of all but a personal ai-node.json).
    fn built(&self, hub_v: &str, id: &str, hub: &str, cfg: Option<&Value>) -> (Arc<Vec<u8>>, String) {
        let stamp = self.stamp();
        if cfg.is_none() { if let Some((h, s, z, v)) = self.cache.lock().unwrap().get(id) { if h == hub && *s == stamp { return (z.clone(), v.clone()); } } }
        let mut entries = self.build(id, hub, cfg);
        let mut all = vec![];
        for en in &entries { if !en.name.ends_with("/ai-node.json") { all.extend(en.name.as_bytes()); all.push(0u8); all.extend(&en.data); } }
        let version = format!("{hub_v}+{}", &vr_core::crypto::sha256_hex(&all)[..8]);
        let folder = entries.first().map(|x| x.name.split('/').next().unwrap_or("").to_string()).unwrap_or_default();
        entries.push(e(format!("{folder}/VERSION.txt"), format!("{version}\n")));
        let z = Arc::new(zip(&entries));
        if cfg.is_none() { self.cache.lock().unwrap().insert(id.into(), (hub.into(), stamp, z.clone(), version.clone())); }
        (z, version)
    }
}
fn hub_version(hub: &Hub) -> String { super::ai_hosts::release_now(&hub.root).0 }
fn hub_url(hub: &Hub) -> String { hub.mods.public_url(&hub.cfg) }
fn zip_answer(z: &[u8], file: &str) -> Routed { Routed::Answer(hyper::Response::builder().status(200).header("content-type", "application/zip").header("content-length", z.len()).header("content-disposition", format!("attachment; filename=\"{file}\"")).header("cache-control", "no-store").body(crate::front::full(z.to_vec())).expect("response")) }
/// /api/pub/*: these were public until 4.0.0; now everything here needs an account (whoever asks, signed in or not).
pub fn gone(r: &Req) -> Option<Routed> {
    if !r.path.starts_with("/api/pub/") { return None; }
    Some(answer(410, json!({ "error": "Downloads and updates need a VibeRoller account now: sign in and use the Download panel (an installed app updates itself while you are signed in)." })))
}
/// Signed in.
pub fn route(hub: &Arc<Hub>, r: &Req, user: &str) -> Option<Routed> {
    let p = r.path.as_str();
    if r.method != hyper::Method::GET { return None; }
    match p {
        "/api/native" => { let (app, target) = (r.param("app").unwrap_or_default(), r.param("target").unwrap_or_default()); return Some(match hub.mods.native.signed(&app, &target, None, &hub.mods.ai.release()) { Some(s) => answer(200, s), None => answer(404, json!({ "error": "Nothing built for that app and target." })) }); }
        "/api/native/file" => {
            let name = r.param("name").unwrap_or_default();
            return Some(match hub.mods.native.file(&r.param("app").unwrap_or_default(), &r.param("target").unwrap_or_default(), &name) {
                Some(buf) => Routed::Answer(hyper::Response::builder().status(200).header("content-type", "application/octet-stream").header("content-length", buf.len()).header("content-disposition", format!("attachment; filename=\"{name}\"")).header("cache-control", "no-store").header("x-content-type-options", "nosniff").body(crate::front::full(buf)).expect("response")),
                None => answer(404, json!({ "error": "Not a file of that release." })),
            });
        }
        _ => {}
    }
    if !(p == "/api/downloads" || p.starts_with("/api/downloads/")) { return None; }
    let d = &hub.mods.downloads; let (hv, url) = (hub_version(hub), hub_url(hub));
    if p == "/api/downloads" {
        let items: Vec<Value> = IDS.iter().filter(|id| d.available(id)).map(|id| { let pk = pkg(id).unwrap(); let (z, v) = d.built(&hv, id, &url, None); json!({ "id": id, "group": pk.group, "os": pk.os, "title": pk.title, "file": pk.file, "bytes": z.len(), "version": v, "native": true }) }).collect();
        return Some(answer(200, json!({ "items": items, "version": hv, "native": hub.mods.native.list(&hub.mods.ai.release()) })));
    }
    let id = p.strip_prefix("/api/downloads/").and_then(|x| x.strip_suffix(".zip")).unwrap_or("");
    let Some(pk) = pkg(id) else { return Some(answer(404, json!({ "error": "No such download." }))) };
    if !d.available(id) { return Some(answer(404, json!({ "error": "Could not build the package: Not built on this hub yet (sh native/build.sh)." }))); }
    let host = r.param("host").filter(|h| !h.is_empty() && pk.group == "native-host");
    let cfg = match host { Some(h) => { let models: Vec<String> = hub.cfg.get("models").and_then(Value::as_array).map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default(); match hub.mods.ai.config_for(user, &h, &url, models) { Ok(c) => Some(c), Err(e) => return Some(answer(500, json!({ "error": format!("Could not build the package: {e}") }))) } } None => None };
    let (z, _) = d.built(&hv, id, &url, cfg.as_ref());
    let file = match &cfg { Some(c) => pk.file.replace(".zip", &format!("-{}.zip", c["nickname"].as_str().unwrap_or(""))), None => pk.file.to_string() };
    Some(zip_answer(&z, &file))
}
