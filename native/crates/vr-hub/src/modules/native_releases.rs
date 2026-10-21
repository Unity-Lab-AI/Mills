//! The native programs' releases (moved from bridge/native-releases.js, which the Download panel still uses): the
//! pre-built binaries in native/dist/<target>/, described in signed manifests. The manifest is the JSON text
//! { app, target, version, commit, files: [{ name, sha256, size }] }, handed out as that exact text plus its signatures:
//! sig (Ed25519 with the release key in bridge/release-key.json) and, for an AI host, hmac (HMAC-SHA256 with its secret).
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-gnu"];
fn app_files(app: &str) -> Option<&'static [&'static str]> { match app { "vr-host" => Some(&["vr-host", "vr-host.exe", "vr-hostw.exe"]), "vr-desktop" => Some(&["vr-desktop", "vr-desktop.exe"]), _ => None } }

pub struct Releases { dist: PathBuf, key_file: PathBuf, cache: Mutex<HashMap<String, (String, String)>> }
impl Releases {
    pub fn new(root: &std::path::Path) -> Releases { Releases { dist: root.join("..").join("native").join("dist"), key_file: root.join("release-key.json"), cache: Mutex::default() } }
    /// The release key (the JavaScript hub makes it on first use; until then nothing is signed here).
    fn key(&self) -> Option<SigningKey> {
        let k: Value = serde_json::from_str(&std::fs::read_to_string(&self.key_file).ok()?).ok()?;
        let seed: [u8; 32] = hex::decode(k["secret"].as_str()?).ok()?.try_into().ok()?;
        Some(SigningKey::from_bytes(&seed))
    }
    /// rel: what this hub runs (version, commit), for a build that did not write them into BUILD.json
    fn manifest_text(&self, app: &str, target: &str, rel: &(String, String)) -> Option<String> {
        let names = app_files(app)?; if !TARGETS.contains(&target) { return None; }
        let dir = self.dist.join(target);
        let present: Vec<&str> = names.iter().copied().filter(|n| dir.join(n).is_file()).collect();
        if present.is_empty() { return None; }
        let stamp = present.iter().map(|n| { let m = std::fs::metadata(dir.join(n)).ok(); format!("{n}:{:?}:{}", m.as_ref().and_then(|m| m.modified().ok()), m.map(|m| m.len()).unwrap_or(0)) }).collect::<Vec<_>>().join("|");
        let ck = format!("{app}|{target}");
        if let Some((s, t)) = self.cache.lock().unwrap().get(&ck) { if *s == stamp { return Some(t.clone()); } }
        let build: Value = std::fs::read_to_string(dir.join("BUILD.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        let pick = |k: &str, d: &str| build[k].as_str().filter(|s| !s.is_empty()).unwrap_or(d).to_string();
        let files: Vec<Value> = present.iter().filter_map(|n| std::fs::read(dir.join(n)).ok().map(|b| json!({ "name": n, "sha256": vr_core::crypto::sha256_hex(&b), "size": b.len() }))).collect();
        let text = json!({ "app": app, "target": target, "version": pick("version", &rel.0), "commit": pick("commit", &rel.1), "files": files }).to_string();
        self.cache.lock().unwrap().insert(ck, (stamp, text.clone()));
        Some(text)
    }
    /// { manifest, sig, hmac? } or None while nothing is built for that app + target.
    pub fn signed(&self, app: &str, target: &str, host_secret: Option<&str>, rel: &(String, String)) -> Option<Value> {
        let text = self.manifest_text(app, target, rel)?; let key = self.key()?;
        let mut v = json!({ "manifest": text, "sig": hex::encode(key.sign(text.as_bytes()).to_bytes()) });
        if let Some(s) = host_secret { v["hmac"] = json!(vr_core::crypto::hmac_hex(s.as_bytes(), text.as_bytes())); }
        Some(v)
    }
    /// What is built, for /api/pub/versions.
    pub fn list(&self, rel: &(String, String)) -> Vec<Value> {
        let mut out = vec![];
        for app in ["vr-host", "vr-desktop"] { for target in TARGETS { if let Some(m) = self.manifest_text(app, target, rel).and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            out.push(json!({ "app": app, "target": target, "version": m["version"], "commit": m["commit"], "files": m["files"].as_array().cloned().unwrap_or_default().iter().map(|f| json!({ "name": f["name"], "size": f["size"], "sha256": f["sha256"] })).collect::<Vec<_>>() }));
        } } }
        out
    }
    pub fn file(&self, app: &str, target: &str, name: &str) -> Option<Vec<u8>> {
        if !TARGETS.contains(&target) || !app_files(app)?.contains(&name) { return None; }
        std::fs::read(self.dist.join(target).join(name)).ok()
    }
}
