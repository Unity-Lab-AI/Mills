//! A program's settings: `<root>/config.json` overlaid with `<root>/config.local.json` (the machine's own, not in git),
//! one level deep: an object in the local file adds to / replaces the keys of the same object, anything else replaces.
use serde_json::{Map, Value};
use std::path::Path;

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
