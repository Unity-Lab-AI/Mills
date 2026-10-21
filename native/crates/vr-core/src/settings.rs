//! ai-node.json: the AI host's settings, the same file the JavaScript host (bridge/ai-node.js) and its control app
//! (bridge/vr-host.js) use. Kept as the JSON object it is, so keys this version does not know (and their order) survive
//! every save; typed getters give each setting with the same default and range the JavaScript side applies.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Clone, Debug)]
pub struct HostSettings { pub path: PathBuf, pub raw: Map<String, Value> }

/// One provider this host may lend (an entry of "providers").
#[derive(Clone, Debug)]
pub struct Provider {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub models: Vec<String>,
    pub enabled: bool,
    pub share_with_group: bool,
    pub allowed_users: Vec<String>,
    pub max5h: Option<f64>,
    pub max7d: Option<f64>,
    pub base_url: String,
    pub api_key: String,
    pub config_dir: String,
    pub codex_home: String,
    pub command: String,
}
impl Provider {
    fn from(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let id = s("id"); if id.is_empty() { return None; }
        let list = |k: &str| v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        Some(Provider { id, kind: s("kind"), label: s("label"), models: list("models"), enabled: v.get("enabled").and_then(Value::as_bool) != Some(false),
            share_with_group: v.get("shareWithGroup").and_then(Value::as_bool) == Some(true), allowed_users: list("allowedUsers"),
            max5h: v.get("max5h").and_then(Value::as_f64), max7d: v.get("max7d").and_then(Value::as_f64),
            base_url: s("baseUrl"), api_key: s("apiKey"), config_dir: s("configDir"), codex_home: s("codexHome"), command: s("command") })
    }
    /// The kind told to the hub: a Claude Code login is lent as a relay, never as a computer (host-relay.js relayKind).
    pub fn relay_kind(&self) -> &str { if self.kind == "claude-cli" { "claude-relay" } else { &self.kind } }
    pub fn is_relay(&self) -> bool { matches!(self.relay_kind(), "claude-relay" | "codex-relay") }
}

fn clamp(v: Option<&Value>, lo: f64, hi: f64, d: f64) -> f64 { v.and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).filter(|n| n.is_finite() && *n >= lo && *n <= hi).unwrap_or(d) }
pub fn yes(v: &str) -> bool { matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "y" | "yes" | "on" | "true") }
fn user_list(v: &str) -> Vec<Value> {
    v.split(|c: char| c == ',' || c.is_whitespace()).map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')).map(Value::String).collect()
}
fn is_local_http(hub: &str) -> bool {
    let rest = match hub.strip_prefix("http://") { Some(r) => r, None => return false };
    let host = rest.split(':').next().unwrap_or("");
    let port_ok = rest.split_once(':').map(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())).unwrap_or(true);
    (host == "127.0.0.1" || host == "localhost" || rest.starts_with("[::1]")) && port_ok && !rest.contains('/')
}

impl HostSettings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&path).map_err(|e| Error::msg(format!("Cannot read {}: {e}", path.display())))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::msg(format!("{} is not valid JSON: {e}", path.display())))?;
        match v { Value::Object(raw) => Ok(HostSettings { path, raw }), _ => Err(Error::msg(format!("{} must hold a JSON object", path.display()))) }
    }
    pub fn new(path: impl AsRef<Path>) -> Self { HostSettings { path: path.as_ref().to_path_buf(), raw: Map::new() } }
    /// Written atomically (a temporary file renamed over it) and readable by its owner only: it holds the token.
    pub fn save(&self) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&Value::Object(self.raw.clone()))? + "\n")?;
        private(&tmp);
        std::fs::rename(&tmp, &self.path)?;
        private(&self.path);
        Ok(())
    }
    fn s(&self, k: &str) -> String { self.raw.get(k).and_then(Value::as_str).unwrap_or("").to_string() }
    fn obj(&self, k: &str) -> Option<&Map<String, Value>> { self.raw.get(k).and_then(Value::as_object) }
    pub fn hub(&self) -> String { self.s("hub").trim_end_matches('/').to_string() }
    pub fn id(&self) -> String { let id = self.s("id"); if id.is_empty() { self.s("name") } else { id } }
    pub fn token(&self) -> String { self.s("token") }
    pub fn user(&self) -> String { self.s("user") }
    pub fn nickname(&self) -> String { let n = self.s("nickname"); if n.is_empty() { self.id() } else { n } }
    pub fn auto_update(&self) -> bool { self.raw.get("autoUpdate").and_then(Value::as_bool) != Some(false) }
    pub fn allow_work(&self) -> bool { self.raw.get("allowWork").and_then(Value::as_bool) == Some(true) }
    pub fn reconnect_max_ms(&self) -> u64 { (clamp(self.obj("reconnect").and_then(|o| o.get("maxDelaySeconds")), 1.0, 60.0, 5.0) * 1000.0) as u64 }
    pub fn update_quiet_ms(&self) -> u64 { (clamp(self.obj("update").and_then(|o| o.get("quietMinutes")), 1.0, 60.0, 3.0) * 60000.0) as u64 }
    pub fn update_check_ms(&self) -> u64 { (clamp(self.obj("update").and_then(|o| o.get("checkMinutes")), 1.0, 60.0, 5.0) * 60000.0) as u64 }
    pub fn allow_insecure_hub(&self) -> bool { self.obj("security").and_then(|o| o.get("allowInsecureHub")).and_then(Value::as_bool) == Some(true) }
    pub fn engine_guard(&self) -> bool { self.obj("security").and_then(|o| o.get("engineGuard")).and_then(Value::as_bool) != Some(false) }
    pub fn pc_folder(&self) -> Option<PathBuf> { self.obj("pc").and_then(|o| o.get("folder")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| std::path::absolute(s).unwrap_or_else(|_| PathBuf::from(s))) }
    pub fn work(&self) -> Map<String, Value> { self.obj("work").cloned().unwrap_or_default() }
    /// The engines lent to the hub by name → their local address (the ones switched off live under "enginesOff").
    pub fn engines(&self) -> Vec<(String, String)> { self.obj("engines").map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|u| (k.clone(), u.to_string()))).collect()).unwrap_or_default() }
    /// Every provider, the switched-off ones too.
    pub fn all_providers(&self) -> Vec<Provider> { self.raw.get("providers").and_then(Value::as_array).map(|a| a.iter().filter_map(Provider::from).collect()).unwrap_or_default() }
    /// The providers offered to the hub ("enabled": false ones are not offered at all).
    pub fn providers(&self) -> Vec<Provider> { self.all_providers().into_iter().filter(|p| p.enabled).collect() }

    /// What the host refuses to start with: no hub / id / token, or a plain-http hub on another machine.
    pub fn check(&self) -> Result<()> {
        let hub = self.hub();
        if !(hub.starts_with("https://") || hub.starts_with("http://")) || self.id().is_empty() || self.token().is_empty() { return Err(Error::msg("ai-node.json needs hub, id (or name) and token.")); }
        if !hub.starts_with("https://") && !is_local_http(&hub) && !self.allow_insecure_hub() {
            return Err(Error::msg(format!("{hub} is not https: the token would cross the network in the clear. Use the hub's https address (or set security.allowInsecureHub in the control app, on a network you trust).")));
        }
        Ok(())
    }

    /// Every setting the control app shows and changes: (key, label, value as shown). Mirrors vr-host.js settingsList.
    pub fn list(&self) -> Vec<(String, String, String)> {
        let mut out = vec![];
        let on = |b: bool| if b { "on".to_string() } else { "off".to_string() };
        out.push(("hub".into(), "Hub address".into(), self.hub()));
        out.push(("token".into(), "Token (secret; never shown)".into(), if self.token().is_empty() { "(not set)".into() } else { "•••••••• (set)".into() }));
        out.push(("autoUpdate".into(), "Automatic updates from the hub".into(), on(self.auto_update())));
        out.push(("update.quietMinutes".into(), "Update / restart only after this many quiet minutes".into(), (self.update_quiet_ms() / 60000).to_string()));
        out.push(("update.checkMinutes".into(), "Look for updates every N minutes".into(), (self.update_check_ms() / 60000).to_string()));
        out.push(("reconnect.maxDelaySeconds".into(), "Reconnect: longest wait between tries (seconds)".into(), (self.reconnect_max_ms() / 1000).to_string()));
        for p in self.all_providers() {
            let name = if p.label.is_empty() { p.id.clone() } else { p.label.clone() };
            out.push((format!("provider.{}.enabled", p.id), format!("{name} ({}): offered", p.kind), on(p.enabled)));
            out.push((format!("provider.{}.shareWithGroup", p.id), format!("{name}: shared with the hub's team group"), if p.share_with_group { "yes".into() } else { "no".into() }));
            out.push((format!("provider.{}.allowedUsers", p.id), format!("{name}: shared with these users (comma list)"), if p.allowed_users.is_empty() { "(none)".into() } else { p.allowed_users.join(", ") }));
            out.push((format!("provider.{}.max5h", p.id), format!("{name}: 5-hour usage cap (%)"), p.max5h.unwrap_or(90.0).to_string()));
            out.push((format!("provider.{}.max7d", p.id), format!("{name}: 7-day usage cap (%)"), p.max7d.unwrap_or(90.0).to_string()));
        }
        let off: Vec<String> = self.obj("enginesOff").map(|o| o.keys().cloned().collect()).unwrap_or_default();
        for (name, _) in self.engines() { out.push((format!("engine.{name}.enabled"), format!("Engine {name} (Unity)"), "on".into())); }
        for name in off { out.push((format!("engine.{name}.enabled"), format!("Engine {name} (Unity)"), "off".into())); }
        out.push(("allowWork".into(), "SECURITY · let Work runs execute ON this machine (off = share AI only)".into(), if self.allow_work() { "ON".into() } else { "off".into() }));
        out.push(("pc.folder".into(), "SECURITY · direct file access for your own Work runs (a folder; empty = off)".into(), self.pc_folder().map(|p| p.display().to_string()).unwrap_or_else(|| "off".into())));
        out.push(("security.allowInsecureHub".into(), "SECURITY · allow a plain-http hub on another machine".into(), if self.allow_insecure_hub() { "ON".into() } else { "off".into() }));
        out.push(("security.engineGuard".into(), "SECURITY · engine guard (the hub may use the engines, never manage them)".into(), on(self.engine_guard())));
        out
    }

    /// Changes one setting by its key (as listed), checked like the control app checks it. Not saved: call save().
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let v = value.trim();
        let range = |lo: f64, hi: f64| -> Result<f64> { v.parse::<f64>().ok().filter(|n| *n >= lo && *n <= hi).ok_or_else(|| Error::msg(format!("{lo} to {hi}."))) };
        let sub = |s: &mut Self, o: &str, k: &str, val: Value| { let e = s.raw.entry(o.to_string()).or_insert_with(|| json!({})); if !e.is_object() { *e = json!({}); } e.as_object_mut().expect("object").insert(k.to_string(), val); };
        match key {
            "hub" => {
                let h = v.trim_end_matches('/');
                let https_ok = h.strip_prefix("https://").map(|r| !r.is_empty() && !r.contains('/') && !r.contains(char::is_whitespace)).unwrap_or(false);
                if !https_ok && !is_local_http(h) { return Err(Error::msg("Give the hub's https:// address (e.g. https://portal.unityailab.com).")); }
                self.raw.insert("hub".into(), json!(h));
            }
            "token" => { if v.len() < 24 { return Err(Error::msg("A token is at least 24 characters (VibeRoller → Settings → My AI hosts).")); } self.raw.insert("token".into(), json!(v)); }
            "autoUpdate" => { self.raw.insert("autoUpdate".into(), json!(yes(v))); }
            "update.quietMinutes" => { let n = range(1.0, 60.0)?; sub(self, "update", "quietMinutes", json!(n)); }
            "update.checkMinutes" => { let n = range(1.0, 60.0)?; sub(self, "update", "checkMinutes", json!(n)); }
            "reconnect.maxDelaySeconds" => { let n = range(1.0, 60.0)?; sub(self, "reconnect", "maxDelaySeconds", json!(n)); }
            "allowWork" => { self.raw.insert("allowWork".into(), json!(yes(v))); }
            "pc.folder" => {
                if v.is_empty() || v.eq_ignore_ascii_case("off") { self.raw.remove("pc"); }
                else { let p = Path::new(v); if !p.is_dir() { return Err(Error::msg("That folder does not exist.")); } sub(self, "pc", "folder", json!(std::path::absolute(p)?.display().to_string())); }
            }
            "security.allowInsecureHub" => sub(self, "security", "allowInsecureHub", json!(yes(v))),
            "security.engineGuard" => sub(self, "security", "engineGuard", json!(yes(v))),
            k if k.starts_with("provider.") => {
                let rest = &k["provider.".len()..];
                let (id, field) = rest.rsplit_once('.').ok_or_else(|| Error::msg(format!("No setting \"{k}\".")))?;
                let val = match field {
                    "enabled" | "shareWithGroup" => json!(yes(v)),
                    "allowedUsers" => Value::Array(user_list(v)),
                    "max5h" | "max7d" => json!(range(1.0, 100.0)?),
                    _ => return Err(Error::msg(format!("No setting \"{k}\"."))),
                };
                let p = self.raw.get_mut("providers").and_then(Value::as_array_mut).and_then(|a| a.iter_mut().find(|p| p.get("id").and_then(Value::as_str) == Some(id)))
                    .ok_or_else(|| Error::msg(format!("No setting \"{k}\".")))?;
                p.as_object_mut().ok_or_else(|| Error::msg("bad provider"))?.insert(field.to_string(), val);
            }
            k if k.starts_with("engine.") && k.ends_with(".enabled") => {
                let name = &k["engine.".len()..k.len() - ".enabled".len()];
                let url = self.obj("engines").and_then(|o| o.get(name)).or_else(|| self.obj("enginesOff").and_then(|o| o.get(name))).cloned().ok_or_else(|| Error::msg(format!("No setting \"{k}\".")))?;
                for o in ["engines", "enginesOff"] { if let Some(m) = self.raw.get_mut(o).and_then(Value::as_object_mut) { m.remove(name); } }
                sub(self, if yes(v) { "engines" } else { "enginesOff" }, name, url);
                if self.obj("enginesOff").map(|o| o.is_empty()).unwrap_or(false) { self.raw.remove("enginesOff"); }
            }
            _ => return Err(Error::msg(format!("No setting \"{key}\". \"vr-host settings\" lists them."))),
        }
        Ok(())
    }
}

/// Only the owner may read it (Unix: 0600; on Windows the control app's "lock down" sets the ACL with icacls).
/// Only its owner may read the file (it holds a token or a key). Linux: mode 600. Windows: its own ACL instead of the
/// folder's (the folder may be C:\VibeRollerAIHost, which every user of the machine can read): the person, SYSTEM (the
/// service) and Administrators, nobody else; done only when someone else could read it.
pub fn private(path: &Path) {
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)); }
    #[cfg(windows)]
    { if readable_by_others(path) { let _ = win_lock(path); } }
}
/// Is the file readable by anyone but its owner? (Linux: group / other bits; Windows: a Users / Everyone /
/// Authenticated Users entry in its ACL.)
pub fn readable_by_others(path: &Path) -> bool {
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::metadata(path).map(|m| m.permissions().mode() & 0o077 != 0).unwrap_or(false) }
    #[cfg(windows)]
    { match icacls(&[&path.display().to_string()]) { Some((true, out)) => ["BUILTIN\\Users", "Everyone", "Authenticated Users", "NT AUTHORITY\\INTERACTIVE"].iter().any(|w| out.contains(w)), _ => false } }
    #[cfg(not(any(unix, windows)))]
    { let _ = path; false }
}
#[cfg(windows)]
fn icacls(args: &[&str]) -> Option<(bool, String)> {
    use std::os::windows::process::CommandExt;
    let o = std::process::Command::new("icacls").args(args).creation_flags(0x0800_0000).stdin(std::process::Stdio::null()).output().ok()?;
    Some((o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)))
}
/// The ACL: inheritance off, full control for the person (when it is a person: a service runs as the machine account),
/// SYSTEM and Administrators (by their well-known ids, so it works on Windows in any language). Err(why) when icacls refused.
#[cfg(windows)]
pub fn win_lock(path: &Path) -> std::result::Result<(), String> {
    let me = std::env::var("USERNAME").unwrap_or_default();
    let dom = std::env::var("USERDOMAIN").unwrap_or_default();
    let p = path.display().to_string();
    let mut args: Vec<String> = vec![p, "/inheritance:r".into(), "/grant:r".into()];
    if !me.is_empty() && !me.ends_with('$') { args.push(if dom.is_empty() { format!("{me}:F") } else { format!("{dom}\\{me}:F") }); }
    args.push("*S-1-5-18:F".into()); // SYSTEM, by its well-known id (the name is translated on other-language Windows)
    args.push("*S-1-5-32-544:F".into()); // Administrators
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match icacls(&a) { Some((true, _)) => Ok(()), Some((false, out)) => Err(out.trim().lines().last().unwrap_or("").to_string()), None => Err("icacls did not start".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> HostSettings {
        let mut s = HostSettings::new(std::env::temp_dir().join(format!("vr-core-test-{}.json", crate::crypto::random_hex(4))));
        s.raw = serde_json::from_value(json!({ "hub": "https://portal.example.org", "id": "abc", "token": "t".repeat(30), "custom": { "kept": true },
            "providers": [{ "id": "claude", "kind": "claude-cli", "label": "My Claude", "models": ["m1"] }, { "id": "off", "kind": "openai", "enabled": false }],
            "engines": { "ollama": "http://127.0.0.1:11434" } })).unwrap();
        s
    }
    #[test]
    fn defaults_and_checks() {
        let s = sample();
        assert_eq!(s.reconnect_max_ms(), 5000);
        assert_eq!(s.update_quiet_ms(), 180000);
        assert!(s.auto_update() && s.engine_guard() && !s.allow_work());
        assert_eq!(s.providers().len(), 1);
        assert_eq!(s.providers()[0].relay_kind(), "claude-relay");
        assert!(s.check().is_ok());
        let mut t = s.clone(); t.raw.insert("hub".into(), json!("http://10.0.0.5:7777"));
        assert!(t.check().is_err());
        t.raw.insert("hub".into(), json!("http://127.0.0.1:7777"));
        assert!(t.check().is_ok());
    }
    #[test]
    fn set_validates_like_the_control_app_and_keeps_unknown_keys() {
        let mut s = sample();
        assert!(s.set("hub", "http://evil.example").is_err());
        assert!(s.set("token", "short").is_err());
        assert!(s.set("update.quietMinutes", "0").is_err());
        s.set("update.quietMinutes", "7").unwrap();
        assert_eq!(s.update_quiet_ms(), 7 * 60000);
        s.set("provider.claude.allowedUsers", "Gee, mills bad!name").unwrap();
        assert_eq!(s.all_providers()[0].allowed_users, vec!["gee", "mills"]);
        s.set("engine.ollama.enabled", "off").unwrap();
        assert!(s.engines().is_empty());
        s.set("engine.ollama.enabled", "on").unwrap();
        assert_eq!(s.engines().len(), 1);
        assert!(s.raw.get("enginesOff").is_none());
        assert!(s.set("nope", "1").is_err());
        s.save().unwrap();
        let back = HostSettings::load(&s.path).unwrap();
        assert_eq!(back.raw["custom"], json!({ "kept": true }));
        assert!(!readable_by_others(&s.path));
        let _ = std::fs::remove_file(&s.path);
    }
}
