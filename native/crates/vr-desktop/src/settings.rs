//! The desktop app's own settings (desktop.json in the app's config folder, private: it can hold a Forgejo token and
//! an API key): which hub, the local projects folder, build commands, the local API proxy, the AI host folder.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

pub const DEFAULT_HUB: &str = match option_env!("VR_DEFAULT_HUB") { Some(h) => h, None => "https://portal.unityailab.com" };

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub hub: String,
    /// the folder with this person's local projects (files, terminal, builds and git work inside it)
    pub projects: String,
    /// named build / run commands per project folder name: { "my-app": [{ "name": "build", "cmd": "cargo build" }] }
    pub builds: serde_json::Map<String, serde_json::Value>,
    /// Forgejo: its address and this person's token (git over https to that host only)
    pub forgejo_url: String,
    pub forgejo_token: String,
    /// the local API proxy: 127.0.0.1:<port>/v1 → the hub's /v1 with this key (Settings → API access on the hub)
    pub proxy_port: u16,
    pub proxy_key: String,
    pub proxy_on: bool,
    /// the AI host this app controls (a folder with vr-host + ai-node.json); empty = <app data>/ai-host
    pub host_dir: String,
    /// close the window to the tray instead of quitting
    pub close_to_tray: bool,
    pub auto_update: bool,
    /// the session of whoever is signed in on the hub's page (set by the page; updates need an account since 4.0.0);
    /// only ever sent to `hub`, and dropped when the hub address changes
    pub hub_session: String,
}
impl Default for Settings {
    fn default() -> Self {
        Settings { hub: DEFAULT_HUB.into(), projects: String::new(), builds: Default::default(), forgejo_url: "https://git.unityailab.com".into(), forgejo_token: String::new(),
            proxy_port: 8789, proxy_key: String::new(), proxy_on: false, host_dir: String::new(), close_to_tray: true, auto_update: true, hub_session: String::new() }
    }
}
/// What the Tools window sees: everything but the secrets (shown as set / not set).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Public { #[serde(flatten)] pub s: Settings, pub has_forgejo_token: bool, pub has_proxy_key: bool }

pub struct Store { pub path: PathBuf, pub s: Mutex<Settings> }
impl Store {
    pub fn load(dir: PathBuf) -> Store {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("desktop.json");
        let s = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Store { path, s: Mutex::new(s) }
    }
    pub fn get(&self) -> Settings { self.s.lock().unwrap().clone() }
    pub fn public(&self) -> Public { let s = self.get(); Public { has_forgejo_token: !s.forgejo_token.is_empty(), has_proxy_key: !s.proxy_key.is_empty(), s: Settings { forgejo_token: String::new(), proxy_key: String::new(), hub_session: String::new(), ..s } } }
    /// The hub's page says who is signed in ('' = signed out).
    pub fn set_session(&self, token: &str) -> Result<(), String> {
        { let mut s = self.s.lock().unwrap(); if s.hub_session == token { return Ok(()); } s.hub_session = token.chars().take(200).collect(); }
        self.save()
    }
    pub fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&*self.s.lock().unwrap()).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        vr_core::settings::private(&tmp);
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        { let me = std::env::var("USERNAME").unwrap_or_default(); let _ = std::process::Command::new("icacls").args([&self.path.display().to_string(), "/inheritance:r", "/grant:r", &format!("{me}:F")]).output(); }
        Ok(())
    }
    /// Applies a partial update from the Tools window ({ key: value }); empty secrets keep the stored ones.
    pub fn patch(&self, v: serde_json::Value) -> Result<(), String> {
        {
            let mut s = self.s.lock().unwrap();
            let mut cur = serde_json::to_value(&*s).map_err(|e| e.to_string())?;
            if let (Some(c), Some(p)) = (cur.as_object_mut(), v.as_object()) {
                for (k, val) in p {
                    if (k == "forgejoToken" || k == "proxyKey") && val.as_str().map(str::is_empty).unwrap_or(false) { continue; }
                    if k == "hubSession" { continue; } // only the hub's page sets it (set_session)
                    c.insert(k.clone(), val.clone());
                }
            }
            let mut next: Settings = serde_json::from_value(cur).map_err(|e| format!("bad setting: {e}"))?;
            if next.hub != s.hub { next.hub_session.clear(); } // a session belongs to its own hub
            if !(next.hub.starts_with("https://") || next.hub.starts_with("http://127.0.0.1") || next.hub.starts_with("http://localhost")) { return Err("The hub address must be https:// (or this machine).".into()); }
            if !next.projects.is_empty() && !std::path::Path::new(&next.projects).is_dir() { return Err("That projects folder does not exist.".into()); }
            *s = next;
        }
        self.save()
    }
}
