//! The hub's side of big-file transfer (plans/linked-machines.md 6.3, T-311).
//!
//! A **ticket** is issued by the hub when a run calls `transfer_get` / `transfer_put`. It is bound to the account, the machine, the
//! path exactly as the AI typed it and the direction, carries a size cap, and is valid for 5 minutes (a call may ask for less,
//! never more). Every machine request that names a ticket is checked against all of those; another account's machine token,
//! another path, another machine or a late request is refused (403 / 410). Tickets live in memory only (a restart ends them).
//!
//! The hub never buffers a file: a chunk (at most a few MB, one request) is appended straight to a part file in the account's
//! transfer area `<root>/transfers/<account>/`, and a read is a range from disk. The area is per account and cleaned after
//! an hour ([`SWEEP_SECS`]). Resume: the part file is found by a key the machine derives from (machine, path, size, mtime), so the
//! same call again carries on at the byte the hub has; the end is a sha256 check over what is on disk.
//! The other end of a transfer is the account owner: they download what a machine sent (`GET /api/transfers/<id>`), and upload
//! what a machine will fetch (`POST /api/transfers`, then chunks, then done).
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// A ticket lives this long unless the call asks for less (seconds).
pub const TICKET_SECS: i64 = 300;
/// Transfer files are kept this long (seconds).
pub const SWEEP_SECS: u64 = 3600;
/// The most one chunk request or range may carry.
pub const MAX_CHUNK: usize = 8 << 20;
const MAX_TICKETS: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir { Get, Put }
impl Dir { pub fn as_str(self) -> &'static str { match self { Dir::Get => "get", Dir::Put => "put" } } }

#[derive(Clone, Debug)]
pub struct Ticket {
    pub id: String,
    pub account: String,
    pub machine: String,
    pub path: String,
    pub dir: Dir,
    pub cap: u64,
    pub expires: i64,
    /// the transfer file it names (a `Put` names it at issue; a `Get` when the machine first says its key)
    pub file: Option<String>,
}

pub type Fail = (u16, String);
fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }
fn now_s() -> i64 { (pgm_core_internal::now_ms() / 1000) as i64 }
fn is_key(k: &str) -> bool { k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) }
fn is_id(k: &str) -> bool { (16..=64).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) }

pub struct Transfers {
    root: PathBuf,
    tickets: Mutex<HashMap<String, Ticket>>,
    pub max_bytes: u64,
}

impl Transfers {
    pub fn new(root: &std::path::Path, max_mb: u64) -> Transfers { Transfers { root: root.join("transfers"), tickets: Mutex::default(), max_bytes: max_mb.max(1) << 20 } }
    pub fn dir_of(&self, account: &str) -> Result<PathBuf, Fail> {
        if account.is_empty() || account.len() > 40 || !account.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') { return Err((403, "Not a valid account for transfers.".into())); }
        let d = self.root.join(account);
        std::fs::create_dir_all(&d).map_err(|e| (500, e.to_string()))?;
        Ok(d)
    }
    fn file(&self, account: &str, id: &str, ext: &str) -> Result<PathBuf, Fail> {
        if !is_id(id) { return Err((404, "No such transfer.".into())); }
        Ok(self.dir_of(account)?.join(if ext.is_empty() { id.to_string() } else { format!("{id}.{ext}") }))
    }

    // ----------------------------------------------------------------------------------------------- tickets

    /// Issues a ticket. `ttl` is capped at [`TICKET_SECS`]; `cap` at the hub's own cap.
    pub fn issue(&self, account: &str, machine: &str, path: &str, dir: Dir, file: Option<String>, cap: Option<u64>, ttl: Option<i64>, now: i64) -> Result<Ticket, Fail> {
        self.dir_of(account)?;
        let mut t = lk(&self.tickets);
        t.retain(|_, k| k.expires >= now);
        if t.len() >= MAX_TICKETS { return Err((429, "Too many transfers are open; try again in a few minutes.".into())); }
        let k = Ticket { id: pgm_core_internal::crypto::random_hex(24), account: account.into(), machine: machine.into(), path: path.into(), dir, cap: cap.unwrap_or(self.max_bytes).min(self.max_bytes), expires: now + ttl.unwrap_or(TICKET_SECS).clamp(1, TICKET_SECS), file };
        t.insert(k.id.clone(), k.clone());
        Ok(k)
    }
    /// The machine's request names a ticket: valid only for the machine, account and path it was issued for, inside its time.
    pub fn check(&self, id: &str, machine: &str, account: &str, path: &str, now: i64) -> Result<Ticket, Fail> {
        let g = lk(&self.tickets);
        let Some(t) = g.get(id) else { return Err((403, "That ticket is not valid.".into())) };
        if t.machine != machine || t.account != account { return Err((403, "That ticket belongs to another machine or account.".into())); }
        if t.expires < now { return Err((410, "That ticket has expired; call the tool again for a new one.".into())); }
        if t.path != path { return Err((403, "That ticket is for another path.".into())); }
        Ok(t.clone())
    }
    /// A `Get` ticket learns the file it fills the first time the machine names its key; afterwards it must be the same.
    pub fn bind(&self, id: &str, key: &str) -> Result<String, Fail> {
        if !is_key(key) { return Err((400, "The transfer key must be 64 hex characters.".into())); }
        let mut g = lk(&self.tickets);
        let t = g.get_mut(id).ok_or((403, "That ticket is not valid.".into()))?;
        match &t.file { Some(f) if t.dir == Dir::Get && f != key => Err((403, "That ticket is for another file.".into())), Some(f) => Ok(f.clone()), None => { t.file = Some(key.to_string()); Ok(key.to_string()) } }
    }

    // ----------------------------------------------------------------------------------------------- the machine's file (Get)

    /// Bytes the hub holds for `key` (the finished file when it is there, else the part).
    pub fn received(&self, account: &str, key: &str) -> Result<u64, Fail> {
        let fin = self.file(account, key, "")?;
        if let Ok(m) = std::fs::metadata(&fin) { return Ok(m.len()); }
        Ok(std::fs::metadata(self.file(account, key, "part")?).map(|m| m.len()).unwrap_or(0))
    }
    /// Appends one chunk. `Err((409, have))` when `offset` is not where the hub is (the machine re-syncs).
    pub fn append(&self, t: &Ticket, key: &str, offset: u64, bytes: &[u8], restart: bool) -> Result<u64, (u16, String, u64)> {
        let e = |(s, m): Fail| (s, m, 0u64);
        if bytes.len() > MAX_CHUNK { return Err((413, "That chunk is too large.".into(), 0)); }
        let part = self.file(&t.account, key, "part").map_err(e)?;
        if restart { let _ = std::fs::remove_file(&part); let _ = std::fs::remove_file(self.file(&t.account, key, "").map_err(e)?); }
        let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if offset != have { return Err((409, "The chunk does not start where the hub is.".into(), have)); }
        if have + bytes.len() as u64 > t.cap { return Err((413, format!("That is more than the {} MB this transfer may carry.", t.cap >> 20), have)); }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&part).map_err(|x| (500, x.to_string(), have))?;
        f.write_all(bytes).map_err(|x| (500, x.to_string(), have))?;
        Ok(have + bytes.len() as u64)
    }
    /// The end: the part must be `size` bytes with this sha256 (read from disk, streaming). Then it becomes the finished file.
    pub fn finish_get(&self, t: &Ticket, key: &str, size: u64, sha: &str, name: &str) -> Result<Value, Fail> {
        let fin = self.file(&t.account, key, "")?;
        let part = self.file(&t.account, key, "part")?;
        let src = if part.exists() { part.clone() } else { fin.clone() };
        let len = std::fs::metadata(&src).map(|m| m.len()).map_err(|_| (409, "The hub has nothing for that transfer.".to_string()))?;
        if len != size { return Err((409, format!("The hub has {len} bytes, not {size}.")));
        }
        let got = hash_file(&src).map_err(|e| (500, e))?;
        if got != sha { let _ = std::fs::remove_file(&src); return Err((422, "The file did not arrive intact (sha256 differs); the hub dropped it.".into())); }
        if src == part { std::fs::rename(&part, &fin).map_err(|e| (500, e.to_string()))?; }
        let meta = json!({ "id": key, "name": name, "size": size, "sha256": sha, "machine": t.machine, "createdAt": now_s(), "complete": true });
        std::fs::write(self.file(&t.account, key, "json")?, meta.to_string()).map_err(|e| (500, e.to_string()))?;
        Ok(json!({ "ok": true, "id": key, "name": name, "size": size, "sha256": sha, "hubPath": fin.display().to_string() }))
    }

    // ----------------------------------------------------------------------------------------------- the owner's files (Put source, Get result)

    pub fn upload_new(&self, account: &str, name: &str) -> Result<Value, Fail> {
        let id = pgm_core_internal::crypto::random_hex(16);
        let name: String = name.chars().filter(|c| !c.is_control() && *c != '/' && *c != '\\').take(120).collect();
        let meta = json!({ "id": id, "name": if name.is_empty() { "upload".to_string() } else { name }, "size": 0, "complete": false, "createdAt": now_s() });
        std::fs::write(self.file(account, &id, "json")?, meta.to_string()).map_err(|e| (500, e.to_string()))?;
        Ok(meta)
    }
    pub fn upload_append(&self, account: &str, id: &str, offset: u64, bytes: &[u8]) -> Result<u64, (u16, String, u64)> {
        let e = |(s, m): Fail| (s, m, 0u64);
        if bytes.len() > MAX_CHUNK { return Err((413, "That chunk is too large.".into(), 0)); }
        if !self.file(account, id, "json").map_err(e)?.exists() { return Err((404, "No such transfer.".into(), 0)); }
        if self.file(account, id, "").map_err(e)?.exists() { return Err((409, "That upload is finished.".into(), 0)); }
        let part = self.file(account, id, "part").map_err(e)?;
        let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if offset != have { return Err((409, "The chunk does not start where the hub is.".into(), have)); }
        if have + bytes.len() as u64 > self.max_bytes { return Err((413, format!("That is more than the {} MB a transfer may carry.", self.max_bytes >> 20), have)); }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&part).map_err(|x| (500, x.to_string(), have))?;
        f.write_all(bytes).map_err(|x| (500, x.to_string(), have))?;
        Ok(have + bytes.len() as u64)
    }
    pub fn upload_done(&self, account: &str, id: &str) -> Result<Value, Fail> {
        let mf = self.file(account, id, "json")?;
        let mut meta: Value = std::fs::read_to_string(&mf).ok().and_then(|t| serde_json::from_str(&t).ok()).ok_or((404, "No such transfer.".to_string()))?;
        let part = self.file(account, id, "part")?;
        let fin = self.file(account, id, "")?;
        if !fin.exists() { std::fs::rename(&part, &fin).map_err(|_| (409, "Nothing was uploaded.".to_string()))?; }
        meta["size"] = json!(std::fs::metadata(&fin).map(|m| m.len()).unwrap_or(0));
        meta["sha256"] = json!(hash_file(&fin).map_err(|e| (500, e))?);
        meta["complete"] = json!(true);
        std::fs::write(&mf, meta.to_string()).map_err(|e| (500, e.to_string()))?;
        Ok(meta)
    }
    pub fn meta(&self, account: &str, id: &str) -> Option<Value> {
        let mut v: Value = std::fs::read_to_string(self.file(account, id, "json").ok()?).ok().and_then(|t| serde_json::from_str(&t).ok())?;
        if v["complete"] != json!(true) { v["received"] = json!(std::fs::metadata(self.file(account, id, "part").ok()?).map(|m| m.len()).unwrap_or(0)); }
        Some(v)
    }
    pub fn list(&self, account: &str) -> Vec<Value> {
        let Ok(d) = self.dir_of(account) else { return vec![] };
        let mut v: Vec<Value> = std::fs::read_dir(d).map(|rd| rd.flatten().filter_map(|e| { let n = e.file_name().to_string_lossy().to_string(); n.strip_suffix(".json").map(|i| i.to_string()) }).filter_map(|id| self.meta(account, &id)).collect()).unwrap_or_default();
        v.sort_by_key(|m| std::cmp::Reverse(m["createdAt"].as_i64().unwrap_or(0)));
        v
    }
    pub fn delete(&self, account: &str, id: &str) -> bool {
        let Ok(m) = self.file(account, id, "json") else { return false };
        let had = m.exists();
        for ext in ["json", "part", ""] { if let Ok(f) = self.file(account, id, ext) { let _ = std::fs::remove_file(f); } }
        had
    }
    /// The finished file for a download: (file, size, name).
    pub fn open(&self, account: &str, id: &str) -> Option<(std::fs::File, u64, String)> {
        let meta = self.meta(account, id)?;
        if meta["complete"] != json!(true) { return None; }
        let f = std::fs::File::open(self.file(account, id, "").ok()?).ok()?;
        let len = f.metadata().ok()?.len();
        Some((f, len, meta["name"].as_str().unwrap_or("file").to_string()))
    }
    /// A range of a finished file (what a `Put` ticket's machine reads).
    pub fn read_range(&self, t: &Ticket, offset: u64, len: usize) -> Result<Vec<u8>, Fail> {
        let id = t.file.clone().ok_or((400, "That ticket names no file.".to_string()))?;
        let (mut f, size, _) = self.open(&t.account, &id).ok_or((404, "The file is no longer kept.".to_string()))?;
        if offset > size { return Err((416, "Past the end of the file.".into())); }
        let n = len.min(MAX_CHUNK).min((size - offset) as usize);
        f.seek(SeekFrom::Start(offset)).map_err(|e| (500, e.to_string()))?;
        let mut buf = vec![0u8; n];
        f.read_exact(&mut buf).map_err(|e| (500, e.to_string()))?;
        Ok(buf)
    }
    /// Files older than an hour go (and tickets that ran out). Returns how many files.
    pub fn sweep(&self) -> usize {
        lk(&self.tickets).retain(|_, k| k.expires >= now_s());
        let mut n = 0;
        let Ok(accounts) = std::fs::read_dir(&self.root) else { return 0 };
        for a in accounts.flatten() {
            let Ok(rd) = std::fs::read_dir(a.path()) else { continue };
            for e in rd.flatten() {
                let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs() > SWEEP_SECS).unwrap_or(false);
                if old && e.path().extension().map(|x| x != "json").unwrap_or(true) { if std::fs::remove_file(e.path()).is_ok() { n += 1; let _ = std::fs::remove_file(e.path().with_extension("json")); } }
            }
        }
        n
    }
}

/// sha256 of a file, read in 1 MB steps.
pub fn hash_file(p: &std::path::Path) -> Result<String, String> {
    let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
    let (mut h, mut buf) = (Sha256::new(), vec![0u8; 1 << 20]);
    loop { let n = f.read(&mut buf).map_err(|e| e.to_string())?; if n == 0 { break; } h.update(&buf[..n]); }
    Ok(hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp() -> PathBuf { let d = std::env::temp_dir().join(format!("vr-xfer-{}", pgm_core_internal::crypto::random_hex(4))); std::fs::create_dir_all(&d).unwrap(); d }

    #[test]
    fn a_ticket_is_bound_to_machine_account_path_and_five_minutes() {
        let d = tmp();
        let x = Transfers::new(&d, 100);
        let now = 1_790_000_000;
        let t = x.issue("sponge", "m1", "big/file.bin", Dir::Get, None, None, None, now).unwrap();
        assert_eq!(t.expires, now + 300);
        assert!(x.check(&t.id, "m1", "sponge", "big/file.bin", now + 299).is_ok());
        assert_eq!(x.check(&t.id, "m1", "sponge", "big/file.bin", now + 301).unwrap_err().0, 410, "after 5 minutes");
        assert_eq!(x.check(&t.id, "m2", "sponge", "big/file.bin", now).unwrap_err().0, 403, "another machine");
        assert_eq!(x.check(&t.id, "m1", "gee", "big/file.bin", now).unwrap_err().0, 403, "another account");
        assert_eq!(x.check(&t.id, "m1", "sponge", "big/other.bin", now).unwrap_err().0, 403, "another path");
        assert_eq!(x.check("nope", "m1", "sponge", "big/file.bin", now).unwrap_err().0, 403);
        // a call may ask for less, never more
        assert_eq!(x.issue("sponge", "m1", "p", Dir::Get, None, None, Some(5), now).unwrap().expires, now + 5);
        assert_eq!(x.issue("sponge", "m1", "p", Dir::Get, None, None, Some(9999), now).unwrap().expires, now + 300);
        assert_eq!(x.issue("sponge", "m1", "p", Dir::Get, None, Some(u64::MAX), None, now).unwrap().cap, 100 << 20);
        assert!(x.issue("../x", "m1", "p", Dir::Get, None, None, None, now).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    #[allow(non_snake_case)]
    fn chunks_append_resume_and_end_with_a_checked_sha256() {
        let key_s = "ab12".repeat(16); let KEY: &str = &key_s;
        let d = tmp();
        let x = Transfers::new(&d, 1);
        let t = x.issue("sponge", "m1", "f", Dir::Get, None, None, None, now_s()).unwrap();
        assert_eq!(x.bind(&t.id, KEY).unwrap(), KEY);
        assert_eq!(x.bind(&t.id, &KEY.replace("ab", "cd")).unwrap_err().0, 403, "a ticket fills one file");
        assert!(x.bind(&t.id, "short").is_err());
        let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(x.received("sponge", KEY).unwrap(), 0);
        assert_eq!(x.append(&t, KEY, 0, &data[..1000], false).unwrap(), 1000);
        assert_eq!(x.append(&t, KEY, 500, &data[500..1500], false).unwrap_err().2, 1000, "out of step: the hub says where it is");
        assert_eq!(x.received("sponge", KEY).unwrap(), 1000, "resume point");
        assert_eq!(x.append(&t, KEY, 1000, &data[1000..], false).unwrap(), 3000);
        let sha = hex::encode(Sha256::digest(&data));
        assert_eq!(x.finish_get(&t, KEY, 3000, &"0".repeat(64), "f").unwrap_err().0, 422, "a wrong sha256 drops the file");
        assert_eq!(x.received("sponge", KEY).unwrap(), 0);
        x.append(&t, KEY, 0, &data, false).unwrap();
        assert_eq!(x.finish_get(&t, KEY, 2999, &sha, "f").unwrap_err().0, 409);
        let ok = x.finish_get(&t, KEY, 3000, &sha, "f.bin").unwrap();
        assert_eq!(ok["sha256"], json!(sha));
        let (mut f, size, name) = x.open("sponge", KEY).unwrap();
        let mut got = vec![]; f.read_to_end(&mut got).unwrap();
        assert!(size == 3000 && got == data && name == "f.bin");
        assert!(x.open("gee", KEY).is_none(), "another account has no such file");
        assert_eq!(x.list("sponge").len(), 1);
        assert!(x.list("gee").is_empty());
        // a cap
        let t2 = x.issue("sponge", "m1", "g", Dir::Get, None, Some(100), None, now_s()).unwrap();
        let k2 = KEY.replace("ab", "ef");
        assert_eq!(x.append(&t2, &k2, 0, &[0u8; 101], false).unwrap_err().0, 413);
        assert!(x.delete("sponge", KEY) && !x.delete("sponge", KEY) && x.list("sponge").is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_owner_uploads_and_a_put_ticket_reads_ranges() {
        let d = tmp();
        let x = Transfers::new(&d, 1);
        let m = x.upload_new("sponge", "my/ file.txt").unwrap();
        let id = m["id"].as_str().unwrap().to_string();
        assert_eq!(m["name"], json!("my file.txt"));
        assert_eq!(x.upload_append("sponge", &id, 0, b"hello ").unwrap(), 6);
        assert_eq!(x.upload_append("sponge", &id, 0, b"x").unwrap_err().2, 6);
        assert_eq!(x.upload_append("gee", &id, 6, b"x").unwrap_err().0, 404, "another account cannot add to it");
        assert_eq!(x.upload_append("sponge", &id, 6, b"world").unwrap(), 11);
        assert!(x.open("sponge", &id).is_none(), "not complete yet");
        let done = x.upload_done("sponge", &id).unwrap();
        assert_eq!(done["size"], json!(11));
        assert_eq!(done["sha256"], json!(hex::encode(Sha256::digest(b"hello world"))));
        let t = x.issue("sponge", "m1", "dst.txt", Dir::Put, Some(id.clone()), None, None, now_s()).unwrap();
        assert_eq!(x.read_range(&t, 6, 100).unwrap(), b"world");
        assert_eq!(x.read_range(&t, 0, 5).unwrap(), b"hello");
        assert_eq!(x.read_range(&t, 12, 5).unwrap_err().0, 416);
        let _ = std::fs::remove_dir_all(d);
    }
}
