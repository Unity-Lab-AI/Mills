//! Files (board T-131, module "files"), moved from bridge/files.js + bridge/linux-accounts.js: the chat's attachments
//! (ciphertext only: the browser encrypts every file, its name / type / key travel inside the encrypted message), each
//! person's Work uploads (in their linked Linux account's ~/uploaded_files/<date>/, else the hub's own area for them),
//! and the Files library (every upload with when, where it went, when it was last used; open, download, edit, run,
//! delete). The same files as before: Chat/files/<id> (+ .json), <profile>/uploads-index.json, the uploads themselves.
//! The chat itself still lives in the JavaScript hub: it tells this module when a message names files (attach), when
//! a message is gone, and answers whether someone may read a message (may-read) before one of its files is handed out.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::auth::Account;
use vr_core::now_ms;

pub const MAX_FILE: usize = 24 * 1024 * 1024;
const DAY: u64 = 86_400_000;

pub fn valid_id(id: &str) -> bool { id.len() == 24 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }
fn decode_uri(s: &str) -> String {
    let b = s.as_bytes(); let mut out = Vec::with_capacity(b.len()); let mut i = 0;
    while i < b.len() { if b[i] == b'%' && i + 2 < b.len() { if let Ok(n) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) { out.push(n); i += 3; continue; } } out.push(b[i]); i += 1; }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}
/// A file name that is safe on every system: no folders, no control or reserved characters, not empty, not too long.
pub fn safe_name(raw: &str) -> String {
    let n = decode_uri(raw).replace('\\', "/");
    let base = n.rsplit('/').next().unwrap_or("").to_string();
    let cleaned: String = base.chars().map(|c| if (c as u32) < 0x20 || "<>:\"/\\|?*\u{7f}".contains(c) { '_' } else { c }).collect();
    let t: String = cleaned.trim_start_matches('.').trim().chars().take(120).collect();
    if t.is_empty() { "file".into() } else { t }
}
pub fn clean_mime(m: &str) -> String {
    let m = m.split(';').next().unwrap_or("").trim();
    let ok = m.split_once('/').map(|(a, b)| !a.is_empty() && !b.is_empty() && m.chars().all(|c| c.is_ascii_alphanumeric() || "./+-".contains(c))).unwrap_or(false);
    if ok { m.to_lowercase() } else { "application/octet-stream".into() }
}
pub fn mime_of(name: &str) -> &'static str {
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png", "jpg" | "jpeg" => "image/jpeg", "gif" => "image/gif", "webp" => "image/webp", "svg" => "image/svg+xml", "mp4" => "video/mp4", "webm" => "video/webm", "mov" => "video/quicktime",
        "mp3" => "audio/mpeg", "wav" => "audio/wav", "ogg" => "audio/ogg", "pdf" => "application/pdf", "zip" => "application/zip", "txt" | "ts" | "rs" | "toml" | "yml" | "yaml" | "c" | "h" | "cpp" | "go" | "java" | "log" => "text/plain",
        "md" => "text/markdown", "json" => "application/json", "js" => "text/javascript", "py" => "text/x-python", "sh" => "text/x-shellscript", "html" => "text/html", "css" => "text/css", "csv" => "text/csv", "xml" => "text/xml",
        _ => "application/octet-stream",
    }
}
fn is_exec(p: &Path) -> bool {
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::metadata(p).map(|m| m.permissions().mode() & 0o100 != 0).unwrap_or(false) }
    #[cfg(not(unix))]
    { let _ = p; false }
}
fn free_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() { return name.to_string(); }
    let p = Path::new(name); let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default(); let base = &name[..name.len() - ext.len()];
    for i in 2..10000 { let n = format!("{base} ({i}){ext}"); if !dir.join(&n).exists() { return n; } }
    format!("{}-{name}", vr_core::crypto::random_hex(4))
}
fn mtime_ms(p: &Path) -> f64 { std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0) }
fn atime_ms(p: &Path) -> f64 { std::fs::metadata(p).and_then(|m| m.accessed()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0) }
fn today() -> String { super::auth::iso_now()[..10].to_string() }

// ---------- the people's Linux accounts on the server (bridge/linux-accounts.js) ----------
pub struct Linux { map: Map<String, Value>, passwd: PathBuf, cache: Mutex<(u64, Map<String, Value>)>, pub hub_user: String, root: PathBuf }
impl Linux {
    pub fn new(root: &Path, cfg: &super::Config) -> Linux {
        let hub_user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
        Linux { map: cfg.get("linuxUsers").and_then(Value::as_object).cloned().unwrap_or_default(), passwd: cfg.get("linuxPasswdFile").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/etc/passwd")), cache: Mutex::new((0, Map::new())), hub_user, root: root.to_path_buf() }
    }
    fn users(&self) -> Map<String, Value> {
        let mut c = self.cache.lock().unwrap();
        if now_ms() - c.0 < 60_000 && c.0 > 0 { return c.1.clone(); }
        let mut m = Map::new();
        for l in std::fs::read_to_string(&self.passwd).unwrap_or_default().lines() {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() >= 7 && f[2].parse::<u64>().map(|u| u >= 1000).unwrap_or(false) && !f[6].ends_with("nologin") && !f[6].ends_with("false") { m.insert(f[0].to_string(), json!({ "home": f[5] })); }
        }
        *c = (now_ms(), m.clone());
        m
    }
    pub fn linux_user_of(&self, u: &str) -> String {
        let lu = match self.map.get(u) { Some(v) => v.as_str().unwrap_or("").to_string(), None => u.to_string() };
        if !lu.is_empty() && self.users().contains_key(&lu) { lu } else { String::new() }
    }
    /// Where this person's uploads go: their linked ~/uploaded_files, else the hub's own area for them.
    pub fn uploads_dir(&self, u: &str, fallback: &Path) -> (PathBuf, bool, String) {
        let lu = self.linux_user_of(u);
        if lu.is_empty() { return (fallback.to_path_buf(), false, lu); }
        let home = PathBuf::from(self.users()[&lu]["home"].as_str().unwrap_or(""));
        let d = home.join("uploaded_files");
        // the real folder in their home, never a link (one could point at the hub's own data, which this hub may write)
        let real = std::fs::symlink_metadata(&d).map(|m| m.is_dir() && !m.file_type().is_symlink()).unwrap_or(false)
            && match (std::fs::canonicalize(&d), std::fs::canonicalize(&home)) { (Ok(c), Ok(h)) => c.starts_with(h), _ => false };
        let writable = real && d.is_dir() && std::fs::metadata(&d).map(|m| !m.permissions().readonly()).unwrap_or(false) && can_write(&d);
        if writable { (d, true, lu) } else { (fallback.to_path_buf(), false, lu) }
    }
    pub fn status(&self, u: &str, fallback: &Path) -> Value {
        let (d, linked, lu) = self.uploads_dir(u, fallback);
        let script = self.root.join("..").join("deploy").join("link-linux-account.sh");
        let script = std::fs::canonicalize(&script).unwrap_or(script);
        json!({ "linuxUser": lu, "linked": linked, "folder": if linked { d.display().to_string() } else { String::new() }, "hubUser": self.hub_user, "sharedwork": "/srv/apps/sharedwork",
            "howToLink": if !lu.is_empty() && !linked { format!("As {lu} on the server (or your AI agent there): sh {}", script.display()) } else { String::new() } })
    }
}
fn can_write(d: &Path) -> bool {
    #[cfg(unix)]
    { use std::os::unix::ffi::OsStrExt; let c = std::ffi::CString::new(d.as_os_str().as_bytes()).unwrap_or_default(); unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) == 0 } }
    #[cfg(not(unix))]
    { d.is_dir() }
}

// ---------- the module ----------
pub struct Files { dir: PathBuf, pub linux: Linux, lock: Mutex<()> }
pub struct Fail { pub status: u16, pub msg: String }
fn fail(status: u16, m: impl Into<String>) -> Fail { Fail { status, msg: m.into() } }

impl Files {
    pub fn new(mills: &Path, root: &Path, cfg: &super::Config) -> Files {
        let dir = mills.join("Chat").join("files");
        let _ = std::fs::create_dir_all(&dir);
        Files { dir, linux: Linux::new(root, cfg), lock: Mutex::new(()) }
    }
    fn index_file(a: &Account) -> PathBuf { a.dir.join("uploads-index.json") }
    pub fn read_index(a: &Account) -> Map<String, Value> { std::fs::read_to_string(Self::index_file(a)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["entries"].as_object().cloned()).unwrap_or_default() }
    fn update_index(&self, a: &Account, f: impl FnOnce(&mut Map<String, Value>)) {
        let _g = self.lock.lock().unwrap();
        let mut e = Self::read_index(a); f(&mut e);
        let file = Self::index_file(a); let tmp = a.dir.join("uploads-index.json.tmp");
        if std::fs::write(&tmp, json!({ "entries": Value::Object(e) }).to_string()).is_ok() { let _ = std::fs::rename(&tmp, &file); }
    }
    fn touch(&self, a: &Account, ids: &[String], use_: Option<Value>) {
        if ids.is_empty() { return; }
        self.update_index(a, |e| for id in ids { if let Some(x) = e.get_mut(id) { x["lastAccess"] = json!(now_ms()); if let Some(u) = &use_ { let mut uses = x["uses"].as_array().cloned().unwrap_or_default(); let mut rec = json!({ "at": now_ms() }); if let (Some(r), Some(o)) = (rec.as_object_mut(), u.as_object()) { r.extend(o.clone()); } uses.push(rec); let n = uses.len(); x["uses"] = json!(uses[n.saturating_sub(50)..].to_vec()); } } });
    }
    // ---- chat: ciphertext only ----
    fn meta(&self, id: &str) -> Option<Value> { std::fs::read_to_string(self.dir.join(format!("{id}.json"))).ok().and_then(|t| serde_json::from_str(&t).ok()) }
    fn set_meta(&self, id: &str, m: &Value) { let _ = std::fs::write(self.dir.join(format!("{id}.json")), m.to_string()); }
    fn remove_cipher(&self, id: &str) { let _ = std::fs::remove_file(self.dir.join(id)); let _ = std::fs::remove_file(self.dir.join(format!("{id}.json"))); }
    pub fn chat_upload(&self, a: &Account, body: &[u8], seal: &str) -> Result<Value, Fail> {
        if body.len() > MAX_FILE + 64 { return Err(fail(413, format!("That file is too large (up to {} MB).", MAX_FILE / 1048576))); }
        if body.is_empty() { return Err(fail(400, "Empty file.")); }
        let id = vr_core::crypto::random_hex(12);
        let f = self.dir.join(&id);
        std::fs::write(&f, body).map_err(|e| fail(500, e.to_string()))?;
        vr_core::settings::private(&f);
        self.set_meta(&id, &json!({ "by": a.username, "at": now_ms(), "size": body.len(), "seq": 0 }));
        let seal_ok = (20..=4000).contains(&seal.len()) && seal.chars().all(|c| c.is_ascii_alphanumeric() || "+/=_.:-".contains(c));
        let (idc, len) = (id.clone(), body.len());
        self.update_index(a, |e| { let mut x = json!({ "id": idc, "kind": "chat", "size": len, "at": now_ms(), "lastAccess": 0 }); if seal_ok { x["seal"] = json!(seal); } e.insert(idc.clone(), x); });
        Ok(json!({ "id": id, "size": body.len() }))
    }
    /// The files a message names: the sender's own, uploaded and not sent yet; marked as that message's.
    pub fn attach(&self, owner: Option<&Account>, user: &str, ids: &Value, seq: i64, conv: &str) -> Result<Vec<String>, Fail> {
        let Some(list) = ids.as_array() else { return if ids.is_null() { Ok(vec![]) } else { Err(fail(400, "Up to 20 files per message.")) } };
        if list.len() > 20 { return Err(fail(400, "Up to 20 files per message.")); }
        let ids: Vec<String> = list.iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect();
        for id in &ids { let ok = valid_id(id) && self.meta(id).map(|m| m["by"] == json!(user) && m["seq"].as_i64().unwrap_or(0) == 0).unwrap_or(false); if !ok { return Err(fail(400, "A file of this message is not yours or was sent already.")); } }
        for id in &ids { let mut m = self.meta(id).unwrap(); m["seq"] = json!(seq); self.set_meta(id, &m); }
        if let Some(a) = owner { let c = if conv.is_empty() { "room" } else { conv }; let ids2 = ids.clone(); self.update_index(a, |e| for id in &ids2 { if let Some(x) = e.get_mut(id) { x["where"] = json!({ "conv": c, "seq": seq, "at": now_ms() }); } }); }
        Ok(ids)
    }
    /// The ciphertext, for its uploader always, for anyone else while they may read its message (`may_read(seq)`).
    /// The ciphertext, when `allowed` (its uploader, or a reader of its message: see owner_seq).
    pub fn fetch(&self, id: &str, owner: Option<Account>, allowed: bool) -> Option<Vec<u8>> {
        if !valid_id(id) || !allowed { return None; }
        let buf = std::fs::read(self.dir.join(id)).ok()?;
        if let Some(a) = owner { self.touch(&a, &[id.to_string()], None); }
        Some(buf)
    }
    /// Who uploaded a chat file and the message it belongs to (0: none).
    pub fn owner_seq(&self, id: &str) -> Option<(String, i64)> { if !valid_id(id) { return None; } let m = self.meta(id)?; Some((m["by"].as_str().unwrap_or("").to_string(), m["seq"].as_i64().unwrap_or(0))) }
    /// A message is gone: its readers lose its files; each stays in its uploader's library when their browser sealed it.
    pub fn message_gone(&self, ids: &[String], owner_of: &dyn Fn(&str) -> Option<Account>) {
        for id in ids {
            if !valid_id(id) { continue; }
            let Some(m) = self.meta(id) else { continue };
            let acct = owner_of(m["by"].as_str().unwrap_or(""));
            let sealed = acct.as_ref().map(|a| Self::read_index(a).get(id).map(|e| e["seal"].is_string()).unwrap_or(false)).unwrap_or(false);
            if !sealed { self.remove_cipher(id); if let Some(a) = &acct { let i = id.clone(); self.update_index(a, |e| { e.remove(&i); }); } continue; }
            let mut m2 = m.clone(); m2["wasSeq"] = m["seq"].clone(); m2["seq"] = json!(0); self.set_meta(id, &m2);
            if let Some(a) = &acct { let i = id.clone(); self.update_index(a, |e| { if let Some(x) = e.get_mut(&i) { if x["where"].is_object() { x["where"]["deleted"] = json!(now_ms()); } } }); }
        }
    }
    pub fn all_gone(&self, owner_of: &dyn Fn(&str) -> Option<Account>) {
        let ids: Vec<String> = std::fs::read_dir(&self.dir).map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".json")).map(|n| n[..n.len() - 5].to_string()).collect()).unwrap_or_default();
        self.message_gone(&ids, owner_of);
    }
    pub fn chat_sweep(&self, owner_of: &dyn Fn(&str) -> Option<Account>) { // uploaded but never sent within a day, and in nobody's library
        let names: Vec<String> = std::fs::read_dir(&self.dir).map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".json")).collect()).unwrap_or_default();
        for n in names {
            let id = &n[..n.len() - 5]; let Some(m) = self.meta(id) else { continue };
            if m["seq"].as_i64().unwrap_or(0) != 0 || !m["wasSeq"].is_null() || now_ms().saturating_sub(m["at"].as_u64().unwrap_or(0)) < DAY { continue; }
            let acct = owner_of(m["by"].as_str().unwrap_or(""));
            let sealed = acct.as_ref().map(|a| Self::read_index(a).get(id).map(|e| e["seal"].is_string()).unwrap_or(false)).unwrap_or(false);
            if !sealed { self.remove_cipher(id); if let Some(a) = &acct { let i = id.to_string(); self.update_index(a, |e| { e.remove(&i); }); } }
        }
    }
    // ---- Work: the person's own uploads ----
    pub fn fallback_dir(a: &Account) -> PathBuf { a.dir.join("uploads") }
    pub fn dir_of(&self, a: &Account) -> PathBuf { self.linux.uploads_dir(&a.username, &Self::fallback_dir(a)).0 }
    pub fn work_upload(&self, a: &Account, body: &[u8], name: &str, mime: &str) -> Result<Value, Fail> {
        if body.len() > MAX_FILE { return Err(fail(413, format!("That file is too large (up to {} MB).", MAX_FILE / 1048576))); }
        if body.is_empty() { return Err(fail(400, "Empty file.")); }
        let id = vr_core::crypto::random_hex(12); let mime = clean_mime(mime);
        let dir = self.dir_of(a).join(today());
        std::fs::create_dir_all(&dir).map_err(|e| fail(500, e.to_string()))?;
        let name = free_name(&dir, &safe_name(name)); let file = dir.join(&name);
        std::fs::write(&file, body).map_err(|e| fail(500, e.to_string()))?;
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o660)); }
        let (idc, n2, m2, fp, len) = (id.clone(), name.clone(), mime.clone(), file.display().to_string(), body.len());
        self.update_index(a, |e| { e.insert(idc.clone(), json!({ "id": idc, "kind": "work", "name": n2, "mime": m2, "size": len, "at": now_ms(), "lastAccess": 0, "path": fp })); });
        Ok(json!({ "id": id, "name": name, "mime": mime, "size": body.len() }))
    }
    /// Files in the uploads folder that VibeRoller did not put there: listed too (id = a hash of the path).
    pub fn on_disk(&self, a: &Account) -> Vec<Value> {
        let root = self.dir_of(a);
        let known: std::collections::HashSet<String> = Self::read_index(a).values().filter_map(|e| e["path"].as_str().map(String::from)).collect();
        let mut out = vec![];
        fn walk(dir: &Path, depth: usize, known: &std::collections::HashSet<String>, out: &mut Vec<Value>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect(); entries.sort_by_key(|e| e.file_name());
            for e in entries {
                if out.len() >= 2000 { return; }
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name.ends_with(".tmp") { continue; }
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() { if depth < 4 && !(depth == 0 && valid_id(&name)) { walk(&p, depth + 1, known, out); } continue; }
                let ps = p.display().to_string();
                if !ft.is_file() || known.contains(&ps) { continue; }
                use sha1::Digest;
                let id = hex::encode(sha1::Sha1::digest(format!("disk:{ps}").as_bytes()))[..24].to_string();
                out.push(json!({ "id": id, "kind": "work", "disk": true, "name": name, "mime": mime_of(&name), "size": e.metadata().map(|m| m.len()).unwrap_or(0), "at": mtime_ms(&p), "lastAccess": atime_ms(&p), "path": ps }));
            }
        }
        walk(&root, 0, &known, &mut out);
        out
    }
    /// { id, name, mime, size, path } of one of this person's uploads.
    pub fn get(&self, a: &Account, id: &str) -> Option<Value> {
        if !valid_id(id) { return None; }
        if let Some(e) = Self::read_index(a).get(id).filter(|e| e["kind"] == "work") { return if Path::new(e["path"].as_str().unwrap_or("")).exists() { Some(e.clone()) } else { None }; }
        if let Some(d) = self.on_disk(a).into_iter().find(|d| d["id"] == json!(id)) { return Some(d); }
        // an upload from before the library (3.7.0: uploads/<id>/<name> with .meta.json)
        let dir = Self::fallback_dir(a).join(id);
        let m: Value = std::fs::read_to_string(dir.join(".meta.json")).ok().and_then(|t| serde_json::from_str(&t).ok())?;
        let f = dir.join(safe_name(m["name"].as_str().unwrap_or("")));
        if f.exists() { Some(json!({ "id": id, "name": m["name"], "mime": m["mime"], "size": m["size"], "path": f.display().to_string() })) } else { None }
    }
    pub fn work_serve(&self, a: &Account, id: &str) -> Option<(Vec<u8>, String, String)> {
        let f = self.get(a, id)?;
        let buf = std::fs::read(f["path"].as_str().unwrap_or("")).ok()?;
        self.touch(a, &[id.to_string()], None);
        Some((buf, f["name"].as_str().unwrap_or("file").to_string(), f["mime"].as_str().unwrap_or("application/octet-stream").to_string()))
    }
    pub fn work_sweep(&self, accounts: &[Account]) { // the pre-library layout kept 30 days
        for a in accounts {
            let root = Self::fallback_dir(a);
            for e in std::fs::read_dir(&root).map(|r| r.filter_map(|e| e.ok()).collect::<Vec<_>>()).unwrap_or_default() {
                let m: Option<Value> = std::fs::read_to_string(e.path().join(".meta.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
                if let Some(m) = m { if now_ms().saturating_sub(m["at"].as_u64().unwrap_or(now_ms())) > 30 * DAY { let _ = std::fs::remove_dir_all(e.path()); } }
            }
        }
    }
    // ---- the library ----
    pub fn list(&self, a: &Account) -> Vec<Value> {
        let mut out: Vec<Value> = Self::read_index(a).values().map(|e| {
            let mut x = e.clone();
            let path = e["path"].as_str().unwrap_or("").to_string();
            let folder = if path.is_empty() { String::new() } else { Path::new(&path).parent().map(|p| p.display().to_string()).unwrap_or_default() };
            let missing = if e["kind"] == "work" { !Path::new(&path).exists() } else { self.meta(e["id"].as_str().unwrap_or("")).is_none() };
            if let Some(o) = x.as_object_mut() { o.remove("path"); o.insert("folder".into(), json!(folder)); o.insert("missing".into(), json!(missing)); o.insert("exec".into(), json!(e["kind"] == "work" && is_exec(Path::new(&path)))); }
            x
        }).collect();
        for d in self.on_disk(a) { let p = d["path"].as_str().unwrap_or("").to_string(); let mut x = d.clone(); if let Some(o) = x.as_object_mut() { o.remove("path"); o.insert("folder".into(), json!(Path::new(&p).parent().map(|q| q.display().to_string()).unwrap_or_default())); o.insert("missing".into(), json!(false)); o.insert("exec".into(), json!(is_exec(Path::new(&p)))); } out.push(x); }
        out.sort_by(|x, y| y["at"].as_f64().unwrap_or(0.0).partial_cmp(&x["at"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
        out
    }
    pub fn remove(&self, a: &Account, id: &str) -> bool {
        let e = Self::read_index(a).get(id).cloned().or_else(|| self.on_disk(a).into_iter().find(|d| d["id"] == json!(id)));
        let Some(e) = e else { return false };
        if e["kind"] == "work" { if let Some(p) = e["path"].as_str() { let _ = std::fs::remove_file(p); if let Some(par) = Path::new(p).parent() { let _ = std::fs::remove_dir(par); } } }
        if e["kind"] == "chat" { if let Some(m) = self.meta(id) { if m["by"] == json!(a.username) { self.remove_cipher(id); } } }
        let i = id.to_string(); self.update_index(a, |x| { x.remove(&i); });
        true
    }
    pub fn save(&self, a: &Account, id: &str, buf: &[u8]) -> Result<Value, Fail> {
        let f = self.get(a, id).ok_or_else(|| fail(404, "Only your own Work files (uploaded_files) can be edited; copy a chat file there first."))?;
        if buf.contains(&0) { return Err(fail(400, "That is not text; only text files are edited here.")); }
        if buf.len() > MAX_FILE { return Err(fail(413, format!("That file is too large (up to {} MB).", MAX_FILE / 1048576))); }
        let p = PathBuf::from(f["path"].as_str().unwrap_or(""));
        let tmp = PathBuf::from(format!("{}.vr-save.tmp", p.display()));
        std::fs::write(&tmp, buf).map_err(|e| fail(500, e.to_string()))?;
        if let Ok(m) = std::fs::metadata(&p) { let _ = std::fs::set_permissions(&tmp, m.permissions()); }
        std::fs::rename(&tmp, &p).map_err(|e| fail(500, e.to_string()))?;
        let (i, n) = (id.to_string(), buf.len());
        self.update_index(a, |e| { if let Some(x) = e.get_mut(&i) { x["size"] = json!(n); x["edited"] = json!(now_ms()); x["lastAccess"] = json!(now_ms()); } });
        Ok(json!({ "id": id, "size": buf.len() }))
    }
    pub fn set_exec(&self, a: &Account, id: &str, on: bool) -> Result<Value, Fail> {
        let f = self.get(a, id).ok_or_else(|| fail(404, "Not found."))?;
        let p = PathBuf::from(f["path"].as_str().unwrap_or(""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o660);
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if on { mode | 0o110 } else { mode & !0o111 }))
                .map_err(|e| fail(403, format!("Could not change it ({}): the file belongs to your own account, so run chmod there.", e.kind())))?;
            Ok(json!({ "id": id, "exec": is_exec(&p) }))
        }
        #[cfg(not(unix))]
        { let _ = (p, on); Err(fail(400, "This hub runs on Windows: files there have no execute permission to set.")) }
    }
    /// A Work message's files (the marker block at its end) → the text Claude gets (with the paths), the files, their folders.
    pub fn work_message(&self, a: &Account, message: &str, where_: Value) -> Value {
        let trimmed = message.trim_end();
        let parsed = (|| { if !trimmed.ends_with(" -->") { return None; } let i = trimmed.rfind("<!--vr-files ")?; let inner = &trimmed[i + "<!--vr-files ".len()..trimmed.len() - 4]; if !inner.starts_with('[') { return None; } Some((i, inner.to_string())) })();
        let Some((i, inner)) = parsed else { return json!({ "text": message, "files": [], "dirs": [] }) };
        let list: Vec<Value> = serde_json::from_str::<Value>(&inner).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let files: Vec<Value> = list.iter().take(20).filter_map(|f| f["id"].as_str().and_then(|id| self.get(a, id))).collect();
        let text = message[..i].trim().to_string();
        if files.is_empty() { return json!({ "text": text, "files": [], "dirs": [] }); }
        let ids: Vec<String> = files.iter().filter_map(|f| f["id"].as_str().map(String::from)).collect();
        let mut use_ = json!({ "where": "work" }); if let (Some(u), Some(w)) = (use_.as_object_mut(), where_.as_object()) { u.extend(w.clone()); }
        self.touch(a, &ids, Some(use_));
        let lines: Vec<String> = files.iter().map(|f| format!("- {}  ({}, {}, {} bytes)", f["path"].as_str().unwrap_or(""), f["name"].as_str().unwrap_or(""), f["mime"].as_str().unwrap_or(""), f["size"])).collect();
        let mut dirs: Vec<String> = vec![]; for f in &files { let d = Path::new(f["path"].as_str().unwrap_or("")).parent().map(|p| p.display().to_string()).unwrap_or_default(); if !dirs.contains(&d) { dirs.push(d); } }
        json!({ "text": format!("{}\n\n[Attached by the user: {} file(s). Read them with the Read tool (it reads pictures too):\n{}]", if text.is_empty() { "(no text: see the attached files)" } else { &text }, files.len(), lines.join("\n")), "files": files, "dirs": dirs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_types() {
        assert_eq!(safe_name("..%2F..%2Fevil%20name.md"), "evil name.md");
        assert_eq!(safe_name("a<b>c?.txt"), "a_b_c_.txt");
        assert_eq!(safe_name(""), "file");
        assert_eq!(clean_mime("text/markdown; charset=utf-8"), "text/markdown");
        assert_eq!(clean_mime("nonsense"), "application/octet-stream");
        assert_eq!(mime_of("x.PY"), "text/x-python");
        assert!(valid_id("0123456789abcdef01234567") && !valid_id("0123"));
    }
}
