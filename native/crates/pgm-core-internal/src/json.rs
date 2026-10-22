//! JSON files and JSON values.
//! - files are written through `<name>.json.tmp` and a rename, so a crash never leaves half a file; pretty-printed
//!   (people read and diff these files)
//! - [`write_json_private`] also makes the file readable by its owner only (it holds tokens, keys or sessions)
//! - [`s`] / [`s_any`]: a field as a string, the two ways the code needs it
use serde_json::Value;
use std::path::Path;

/// The file as JSON, or None (missing, empty, unreadable). A file that is there but is not JSON is bad data: it is moved
/// aside as `<file>.bad-<yyyymmdd-hhmmss>` (kept, never deleted, never overwritten by the next save), a notice says so
/// ([`crate::notices`]), and the caller goes on with its defaults (FR2, VibeRoller's plans/full-rust-hub.md).
pub fn read_json(p: &Path) -> Option<Value> {
    let t = std::fs::read_to_string(p).ok()?;
    if t.trim().is_empty() { return None; }
    match serde_json::from_str(&t) {
        Ok(v) => Some(v),
        Err(e) => {
            let stamp = crate::time::iso_now().replace(['-', ':'], "").replace('T', "-").trim_end_matches('Z').chars().take(15).collect::<String>();
            let aside = p.with_file_name(format!("{}.bad-{stamp}", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
            let shown = p.iter().rev().take(2).map(|x| x.to_string_lossy().to_string()).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/");
            match std::fs::rename(p, &aside) {
                Ok(()) => crate::notices::push(format!("**A data file could not be read.** `{shown}` is not valid JSON ({e}); it was moved to `{}` (kept) and the program went on without it, with defaults. If it held something important, fix it and put it back.", aside.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())),
                Err(re) => crate::notices::push(format!("**A data file could not be read.** `{shown}` is not valid JSON ({e}) and could not be moved aside ({re}); the program went on without it, with defaults.")),
            }
            None
        }
    }
}
/// Written atomically (temp file + rename), pretty.
pub fn write_json(p: &Path, v: &Value) { let tmp = p.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp, p); } }
/// Written atomically and readable by its owner only.
pub fn write_json_private(p: &Path, v: &Value) { let tmp = p.with_extension("json.tmp"); if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() { crate::fs::private(&tmp); let _ = std::fs::rename(&tmp, p); } }
/// A string field ("" when it is missing or not a string).
pub fn s(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_string() }
/// A field as text: a string as it is, null as "", anything else as its JSON.
pub fn s_any(v: &Value, k: &str) -> String { match &v[k] { Value::String(x) => x.clone(), Value::Null => String::new(), o => o.to_string() } }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn files_and_fields() {
        let f = std::env::temp_dir().join(format!("pgm-core-json-{}.json", crate::crypto::random_hex(4)));
        write_json_private(&f, &json!({ "a": 1, "b": "x" }));
        let v = read_json(&f).unwrap();
        assert_eq!((s(&v, "b"), s(&v, "a"), s_any(&v, "a"), s_any(&v, "none")), ("x".to_string(), String::new(), "1".to_string(), String::new()));
        assert!(!crate::fs::readable_by_others(&f));
        let _ = std::fs::remove_file(&f);
    }
}
