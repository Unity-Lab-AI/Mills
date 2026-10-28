//! Team notes (board T-131, module "notes"), moved from bridge/notes.js: markdown files in folders under
//! VibeRoller/Notes (real .md files, also usable outside VibeRoller), shared by the team and by Claude. Not end-to-end
//! encrypted (Claude reads them). Names are case-insensitive (Windows and Linux agree); line endings stored as "\n".
//! Who saved a note last: Notes/.index.json (path → { by, at }), the same file the JavaScript module kept.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Notes { dir: PathBuf, meta: Mutex<Map<String, Value>>, pub ver: tokio::sync::watch::Sender<u64> }
pub struct Err { pub status: u16, pub msg: String }
fn e400(m: impl Into<String>) -> Err { Err { status: 400, msg: m.into() } }
fn e(status: u16, m: impl Into<String>) -> Err { Err { status, msg: m.into() } }
fn seg_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b.len() <= 80 && b[0].is_ascii_alphanumeric() && s.chars().all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c))
}
fn mtime(p: &Path) -> f64 { std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0) }

impl Notes {
    pub fn new(dir: PathBuf) -> Notes {
        let _ = std::fs::create_dir_all(&dir);
        let meta = std::fs::read_to_string(dir.join(".index.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default();
        let (ver, _) = tokio::sync::watch::channel(1u64);
        Notes { dir, meta: Mutex::new(meta), ver }
    }
    pub fn version(&self) -> u64 { *self.ver.borrow() }
    fn save_meta(&self) {
        let f = self.dir.join(".index.json"); let tmp = self.dir.join(".index.json.tmp");
        if std::fs::write(&tmp, Value::Object(self.meta.lock().unwrap_or_else(|e| e.into_inner()).clone()).to_string()).is_ok() { let _ = std::fs::rename(&tmp, &f); }
        self.ver.send_modify(|v| *v += 1);
    }
    fn segs(&self, p: &str, note: bool, folder: bool) -> Result<Vec<String>, Err> {
        let parts: Vec<String> = p.replace('\\', "/").split('/').filter(|s| !s.is_empty()).map(String::from).collect();
        if parts.iter().any(|s| s == "." || s == ".." || !seg_ok(s) || s.starts_with('.')) { return Err(e400("Bad path: folders and notes use letters, digits, spaces, dots, dashes and underscores.")); }
        if note && (parts.is_empty() || !parts.last().unwrap().to_lowercase().ends_with(".md")) { return Err(e400("A note ends in .md")); }
        if folder && parts.is_empty() { return Err(e400("Name the folder.")); }
        Ok(parts)
    }
    fn abs(&self, parts: &[String]) -> PathBuf { parts.iter().fold(self.dir.clone(), |p, s| p.join(s)) }
    /// The on-disk entry whose name matches case-insensitively.
    fn existing(&self, parts: &[String]) -> Option<Vec<String>> {
        let mut cur = self.dir.clone(); let mut out = vec![];
        for s in parts {
            let hit = std::fs::read_dir(&cur).ok()?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).find(|n| n.to_lowercase() == s.to_lowercase())?;
            cur = cur.join(&hit); out.push(hit);
        }
        Some(out)
    }
    pub fn tree(&self, folder: &[String]) -> Vec<Value> {
        let base = self.abs(folder);
        let Ok(rd) = std::fs::read_dir(&base) else { return vec![] };
        let mut es: Vec<(String, bool)> = rd.filter_map(|e| e.ok()).map(|e| (e.file_name().to_string_lossy().to_string(), e.path().is_dir())).filter(|(n, d)| !n.starts_with('.') && (*d || n.to_lowercase().ends_with(".md"))).collect();
        es.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| locale_cmp(&a.0, &b.0)));
        let meta = self.meta.lock().unwrap_or_else(|e| e.into_inner()).clone();
        es.into_iter().map(|(name, dir)| {
            let mut sub = folder.to_vec(); sub.push(name.clone());
            let p = sub.join("/");
            if dir { json!({ "type": "folder", "path": p, "name": name, "children": self.tree(&sub) }) }
            else {
                let f = base.join(&name); let m = meta.get(&p).cloned().unwrap_or(Value::Null);
                json!({ "type": "note", "path": p, "name": name[..name.len() - 3].to_string(), "size": std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0), "at": mtime(&f), "by": m["by"].as_str().unwrap_or("") })
            }
        }).collect()
    }
    pub fn read(&self, p: &str) -> Result<Value, Err> {
        let parts = self.segs(p, true, false)?;
        let hit = self.existing(&parts).ok_or_else(|| e(404, "No such note."))?;
        let f = self.abs(&hit); let rel = hit.join("/");
        let text = std::fs::read_to_string(&f).map_err(|x| e400(x.to_string()))?.replace("\r\n", "\n").replace('\r', "\n");
        let by = self.meta.lock().unwrap_or_else(|e| e.into_inner()).get(&rel).and_then(|m| m["by"].as_str().map(String::from)).unwrap_or_default();
        Ok(json!({ "path": rel, "text": text, "at": mtime(&f), "by": by }))
    }
    pub fn write(&self, actor: &str, p: &str, text: &str, base_at: Option<f64>) -> Result<Value, Err> {
        let parts = self.segs(p, true, false)?;
        let hit = self.existing(&parts);
        if let Some(h) = &hit { if h.join("/") != parts.join("/") { return Err(e400(format!("\"{}\" exists already (names are the same ignoring case; pick another).", h.join("/")))); } }
        let f = self.abs(&parts);
        if let (Some(_), Some(b)) = (&hit, base_at) { if (mtime(&f) - b).abs() > 1.0 { return Err(e(409, "Someone saved this note after you opened it. Reload it, then save again.")); } }
        let body = text.replace("\r\n", "\n").replace('\r', "\n");
        if body.chars().count() > 2_000_000 { return Err(e400("That note is too large (2 MB).")); }
        if let Some(d) = f.parent() { std::fs::create_dir_all(d).map_err(|x| e400(x.to_string()))?; }
        let tmp = PathBuf::from(format!("{}.tmp", f.display()));
        std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &f)).map_err(|x| e400(x.to_string()))?;
        let rel = parts.join("/");
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).insert(rel.clone(), json!({ "by": actor, "at": pgm_core_internal::now_ms() }));
        self.save_meta();
        Ok(json!({ "path": rel, "at": mtime(&f), "by": actor, "created": hit.is_none() }))
    }
    pub fn remove(&self, p: &str) -> Result<Value, Err> {
        let parts = self.segs(p, false, false)?;
        let hit = self.existing(&parts).ok_or_else(|| e(404, "Nothing there."))?;
        let target = self.abs(&hit);
        if target.is_dir() { if std::fs::read_dir(&target).map(|mut r| r.next().is_some()).unwrap_or(false) { return Err(e400("Empty the folder first.")); } std::fs::remove_dir(&target).map_err(|x| e400(x.to_string()))?; }
        else { std::fs::remove_file(&target).map_err(|x| e400(x.to_string()))?; }
        let rel = hit.join("/");
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).remove(&rel); self.save_meta();
        Ok(json!({ "removed": rel }))
    }
    pub fn mv(&self, actor: &str, from: &str, to: &str) -> Result<Value, Err> {
        let a = self.existing(&self.segs(from, false, false)?).ok_or_else(|| e(404, "No such note or folder."))?;
        let is_dir = self.abs(&a).is_dir();
        let b = self.segs(to, !is_dir, is_dir)?;
        if let Some(bh) = self.existing(&b) { if bh.join("/").to_lowercase() != a.join("/").to_lowercase() { return Err(e400(format!("\"{}\" exists already.", bh.join("/")))); } }
        if let Some(d) = self.abs(&b).parent() { let _ = std::fs::create_dir_all(d); }
        std::fs::rename(self.abs(&a), self.abs(&b)).map_err(|x| e400(x.to_string()))?;
        let (ra, rb) = (a.join("/"), b.join("/"));
        {
            let mut meta = self.meta.lock().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<String> = meta.keys().filter(|k| **k == ra || k.starts_with(&format!("{ra}/"))).cloned().collect();
            for k in keys { let mut v = meta.remove(&k).unwrap_or(json!({})); v["by"] = json!(actor); v["at"] = json!(pgm_core_internal::now_ms()); meta.insert(format!("{rb}{}", &k[ra.len()..]), v); }
        }
        self.save_meta();
        Ok(json!({ "from": ra, "to": rb }))
    }
    pub fn folder(&self, p: &str) -> Result<Value, Err> {
        let parts = self.segs(p, false, true)?;
        if self.existing(&parts).is_some() { return Err(e400("That folder exists already.")); }
        std::fs::create_dir_all(self.abs(&parts)).map_err(|x| e400(x.to_string()))?;
        self.ver.send_modify(|v| *v += 1);
        Ok(json!({ "path": parts.join("/") }))
    }
    /// Every word must appear (any case) in the path or the text; a snippet around the first hit.
    pub fn search(&self, q: &str) -> Vec<Value> {
        let words: Vec<String> = q.to_lowercase().split_whitespace().take(8).map(String::from).collect();
        let mut all = vec![];
        fn walk(n: &Notes, items: Vec<Value>, out: &mut Vec<Value>) { for e in items { if e["type"] == "folder" { let sub: Vec<String> = e["path"].as_str().unwrap_or("").split('/').map(String::from).collect(); walk(n, n.tree(&sub), out); } else { out.push(e); } } }
        walk(self, self.tree(&[]), &mut all);
        if words.is_empty() { return all.into_iter().map(|mut n| { n["snippet"] = json!(""); n }).collect(); }
        let mut hits = vec![];
        for mut n in all {
            let path = n["path"].as_str().unwrap_or("").to_string();
            let text = std::fs::read_to_string(self.abs(&path.split('/').map(String::from).collect::<Vec<_>>())).unwrap_or_default();
            let low = format!("{}\n{}", path, text).to_lowercase();
            if !words.iter().all(|w| low.contains(w.as_str())) { continue; }
            let tl = text.to_lowercase();
            // positions in characters, as JavaScript counts them
            let first = words.iter().filter_map(|w| tl.find(w.as_str()).map(|b| tl[..b].chars().count())).min();
            n["snippet"] = json!(match first {
                Some(i) => { let start = i.saturating_sub(60); let s: String = text.chars().skip(start).take(160).collect(); format!("{}{}…", if start > 0 { "…" } else { "" }, s.split_whitespace().collect::<Vec<_>>().join(" ")) }
                None => String::new(),
            });
            hits.push(n);
        }
        hits
    }
    // ---------- for a sync with another copy of the notes (T-324: a dev hub next to the live one) ----------
    /// Every note as (path, { text, mtime (ms), by }).
    pub fn sync_list(&self) -> Vec<(String, Value)> {
        let mut out = vec![];
        fn walk(n: &Notes, sub: &[String], out: &mut Vec<(String, Value)>) {
            let Ok(rd) = std::fs::read_dir(n.abs(sub)) else { return };
            for e in rd.filter_map(|e| e.ok()) {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || !seg_ok(&name) { continue; }
                let mut p = sub.to_vec(); p.push(name.clone());
                if e.path().is_dir() { walk(n, &p, out); } else if name.to_lowercase().ends_with(".md") { let rel = p.join("/"); if let Some(v) = n.sync_get(&rel) { out.push((rel, v)); } }
            }
        }
        walk(self, &[], &mut out);
        out
    }
    /// One note as { text, mtime, by }, None when there is none at that path (exactly; names differing in case are
    /// other notes here).
    pub fn sync_get(&self, rel: &str) -> Option<Value> {
        let parts = self.segs(rel, true, false).ok()?;
        let f = self.abs(&parts);
        if !f.is_file() || self.existing(&parts)?.join("/") != parts.join("/") { return None; }
        let text = std::fs::read_to_string(&f).ok()?.replace("\r\n", "\n").replace('\r', "\n");
        let by = self.meta.lock().unwrap_or_else(|e| e.into_inner()).get(&parts.join("/")).and_then(|m| m["by"].as_str().map(String::from)).unwrap_or_default();
        Some(json!({ "text": text, "mtime": mtime(&f).floor() as i64, "by": by }))
    }
    /// Whether a path is one a note may have.
    pub fn sync_path_ok(&self, rel: &str) -> bool { self.segs(rel, true, false).map(|p| p.join("/") == rel).unwrap_or(false) }
    /// Another copy's version of a note: written with its time and who saved it (pages hear of it as of any save).
    pub fn sync_put(&self, rel: &str, text: &str, mtime_ms: i64, by: &str) -> Result<(), String> {
        let parts = self.segs(rel, true, false).map_err(|e| e.msg)?;
        let f = self.abs(&parts);
        if let Some(d) = f.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
        let tmp = PathBuf::from(format!("{}.tmp", f.display()));
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        if let Ok(h) = std::fs::File::options().write(true).open(&tmp) { let _ = h.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(mtime_ms.max(0) as u64)); }
        std::fs::rename(&tmp, &f).map_err(|e| e.to_string())?;
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).insert(parts.join("/"), json!({ "by": by, "at": mtime_ms }));
        self.save_meta();
        Ok(())
    }
    /// Another copy deleted a note.
    pub fn sync_remove(&self, rel: &str) -> Result<(), String> {
        let parts = self.segs(rel, true, false).map_err(|e| e.msg)?;
        let f = self.abs(&parts);
        if f.is_file() { std::fs::remove_file(&f).map_err(|e| e.to_string())?; }
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).remove(&parts.join("/"));
        self.save_meta();
        Ok(())
    }
    /// Where the losing side of a sync conflict on a note goes: "<name> - <copy> copy.md" next to it (T-324; a note's
    /// name has letters, digits, spaces, dots, dashes and underscores only, so no brackets), shortened to fit 80 characters.
    pub fn sync_copy_path(rel: &str, copy: &str) -> String {
        let copy: String = copy.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(20).collect();
        let tail = format!(" - {} copy.md", if copy.is_empty() { "other" } else { &copy });
        let (dir, name) = match rel.rsplit_once('/') { Some((d, n)) => (format!("{d}/"), n), None => (String::new(), rel) };
        let stem = name.strip_suffix(".md").or_else(|| name.strip_suffix(".MD")).unwrap_or(name);
        let stem: String = stem.chars().take(80usize.saturating_sub(tail.len())).collect();
        format!("{dir}{}{tail}", stem.trim_end())
    }
    /// Claude in a Work run, as "<person> · Claude" (bridge/notes.js call).
    pub fn tool(&self, name: &str, args: &Value, who: &str) -> Value {
        let r = match name {
            "notes_list" => Ok(json!({ "notes": self.search(args["q"].as_str().unwrap_or("")).into_iter().map(|n| { let at = n["at"].as_f64().unwrap_or(0.0) as i64; let mut o = json!({ "path": n["path"], "by": n["by"], "at": pgm_core_internal::time::iso_at(at), "size": n["size"] }); if !n["snippet"].as_str().unwrap_or("").is_empty() { o["snippet"] = n["snippet"].clone(); } o }).collect::<Vec<_>>() })),
            "notes_read" => self.read(args["path"].as_str().unwrap_or("")),
            "notes_write" => self.write(&format!("{who} · Claude"), args["path"].as_str().unwrap_or(""), args["text"].as_str().unwrap_or(""), None),
            "notes_delete" => self.remove(args["path"].as_str().unwrap_or("")),
            _ => return json!({ "ok": false, "error": format!("Unknown notes tool: {name}") }),
        };
        match r { Ok(v) => json!({ "ok": true, "result": v }), Err(x) => json!({ "ok": false, "error": x.msg }) }
    }
}
/// JavaScript's localeCompare for file names, near enough: letters compared ignoring case first.
fn locale_cmp(a: &str, b: &str) -> std::cmp::Ordering { a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| b.cmp(a)) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn another_copys_note_keeps_its_time_and_author_and_a_conflict_copy_is_a_valid_note_name() {
        let dir = std::env::temp_dir().join(format!("pgm-notes-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let n = Notes::new(dir.clone());
        let v0 = n.version();
        n.sync_put("plans/a b.md", "hello\n", 1_700_000_000_123, "Ann").unwrap();
        assert!(n.version() > v0, "pages hear of it");
        assert_eq!(n.sync_get("plans/a b.md").unwrap(), json!({ "text": "hello\n", "mtime": 1_700_000_000_123i64, "by": "Ann" }));
        assert_eq!(n.sync_list().len(), 1);
        let copy = Notes::sync_copy_path("plans/a b.md", "dev");
        assert_eq!(copy, "plans/a b - dev copy.md");
        assert!(n.sync_path_ok(&copy), "the copy's name is one a note may have");
        let long = format!("{}.md", "x".repeat(77));
        assert!(n.sync_path_ok(&Notes::sync_copy_path(&long, "live")), "shortened to fit");
        n.sync_remove("plans/a b.md").unwrap();
        assert!(n.sync_get("plans/a b.md").is_none() && n.sync_list().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
