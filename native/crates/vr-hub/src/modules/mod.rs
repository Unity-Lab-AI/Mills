//! The hub's modules that run here in Rust. Each one owns its routes and its data files outright (the JavaScript hub no
//! longer touches them); where the JavaScript hub still needs one of them, it asks it over the internal API
//! (/internal/front/…, with the front's secret). The order they move in: board T-131.
use hyper::Response;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::Arc;

use crate::front::{json as jresp, Body, Hub, Req};

/// bridge/config.json overlaid with bridge/config.local.json, one level deep (bridge/common.js loadProfile).
#[derive(Clone, Debug)]
pub struct Config { pub raw: Map<String, Value>, pub port: u16 }
impl Config {
    pub fn load(root: &Path) -> Config {
        let read = |f: &str| -> Map<String, Value> { std::fs::read_to_string(root.join(f)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default() };
        let mut base = read("config.json");
        for (k, v) in read("config.local.json") {
            match (base.get_mut(&k), &v) { (Some(Value::Object(b)), Value::Object(l)) => { for (k2, v2) in l { b.insert(k2.clone(), v2.clone()); } } _ => { base.insert(k, v); } }
        }
        let port = base.get("port").and_then(Value::as_u64).unwrap_or(7777) as u16;
        Config { raw: base, port }
    }
    pub fn get(&self, k: &str) -> Option<&Value> { self.raw.get(k) }
}

pub enum Routed { Answer(Response<Body>), Pass(Vec<(String, String)>), No }

pub struct Modules { pub front_secret: String }
impl Modules {
    pub fn new(_root: &Path, _cfg: &Config) -> Result<Modules, String> {
        let front_secret = std::env::var("VR_FRONT_SECRET").ok().filter(|s| s.len() >= 32).unwrap_or_else(|| vr_core::crypto::random_hex(24));
        Ok(Modules { front_secret })
    }
}
pub fn start_background(_hub: &Arc<Hub>) {}

pub async fn route(hub: &Arc<Hub>, r: &Req) -> Routed {
    // the front itself: who answers here (for the tools and the regression suite; nothing private)
    if r.path == "/__vr-front" { return Routed::Answer(jresp(200, &json!({ "front": "vr-hub", "version": crate::VERSION, "modules": native_modules(hub) }))); }
    Routed::No
}
/// The modules this front answers itself.
pub fn native_modules(_hub: &Arc<Hub>) -> Vec<&'static str> { vec![] }
