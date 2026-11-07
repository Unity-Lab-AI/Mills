//! The security log (T-271): an append-only, hash-chained JSON-lines file of the security-relevant actions (sign-ins,
//! new devices, admin changes, provider shares, downloads, config switches, delegation). Separate from the activity
//! history, which is about work.
//! - one line per event: `{seq, at, actor, action, target, detail, ip?, prev, hash}`; `prev` is the line before's
//!   `hash` (64 zeros for the first), `hash` = sha256(prev + the line without its `hash`), hex. A changed, removed or
//!   reordered line breaks the chain from there on, and [`Log::verify`] names the first broken `seq`.
//! - writes go through a lock (in the process, and a lock file beside the log for a second process on the same file)
//!   and are synced to disk; a crash mid-write leaves a torn last line: [`Log::verify`] reports it (`torn`) without
//!   failing the chain, and the next [`Log::record`] moves it aside (`<file>.torn-<ms>`) and notes `audit.torn` first.
//! - secrets never go in: detail keys that look like one (password, token, secret, cookie, authorization, *key) are
//!   dropped, strings are cut short.
//! - [`init`] names the process's log once (the hub: `<bridge>/security/audit.jsonl`); [`record`] writes to it and
//!   is a no-op before (a library that records works in programs that keep no log).
use serde_json::{json, Map, Value};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The `prev` of the first line.
pub const GENESIS: &str = concat!("00000000", "00000000", "00000000", "00000000", "00000000", "00000000", "00000000", "00000000");
const TAIL: u64 = 64 * 1024;
const HASH_SUFFIX: usize = 9 + 64 + 2; // ,"hash":"<64 hex>"}

pub struct Log { path: PathBuf, mx: Mutex<()> }

/// What [`Log::verify`] found: `ok` = every complete line chains; `broken_at` = the first `seq` that does not;
/// `torn` = the file ends in a line cut short (a crash mid-write; not a break).
#[derive(Debug, Clone, PartialEq)]
pub struct Verified { pub ok: bool, pub broken_at: Option<u64>, pub entries: u64, pub torn: bool, pub why: Option<String> }
impl Verified {
    pub fn to_json(&self) -> Value {
        let mut v = json!({ "ok": self.ok, "entries": self.entries, "torn": self.torn });
        if let Some(b) = self.broken_at { v["brokenAt"] = json!(b); }
        if let Some(w) = &self.why { v["why"] = json!(w); }
        v
    }
}

static GLOBAL: OnceLock<Log> = OnceLock::new();
/// This process's security log (the first call wins).
pub fn init(path: &Path) { let _ = GLOBAL.set(Log::open(path)); }
pub fn global() -> Option<&'static Log> { GLOBAL.get() }
/// One event into this process's log (nothing when [`init`] was not called).
pub fn record(actor: &str, action: &str, target: &str, detail: Value, ip: &str) {
    if let Some(l) = global() { if let Err(e) = l.record(actor, action, target, detail, ip) { eprintln!("[audit] could not write the security log: {e}"); } }
}

fn cut(s: &str, n: usize) -> String { s.chars().filter(|c| !c.is_control()).take(n).collect() }
fn secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["password", "passwd", "token", "secret", "cookie", "authorization", "credential", "privatekey"].iter().any(|w| k.contains(w)) || k.ends_with("key")
}
/// The detail as it may be kept: no secret-looking keys (at any depth), strings at most 300 characters, lists at most 50.
pub fn scrub(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().filter(|(k, _)| !secret_key(k)).map(|(k, x)| (cut(k, 64), scrub(x))).collect()),
        Value::Array(a) => Value::Array(a.iter().take(50).map(scrub).collect()),
        Value::String(s) => json!(cut(s, 300)),
        x => x.clone(),
    }
}
fn chain_hash(prev: &str, canonical: &str) -> String {
    let mut b = Vec::with_capacity(prev.len() + canonical.len()); b.extend_from_slice(prev.as_bytes()); b.extend_from_slice(canonical.as_bytes());
    crate::crypto::sha256_hex(&b)
}
/// A stored line → (the line without its hash, the hash it says), when it has the shape the writer gives it.
fn split_line(line: &str) -> Option<(String, &str)> {
    if line.len() <= HASH_SUFFIX || !line.is_char_boundary(line.len() - HASH_SUFFIX) { return None; }
    let (head, tail) = line.split_at(line.len() - HASH_SUFFIX);
    let h = tail.strip_prefix(",\"hash\":\"")?.strip_suffix("\"}")?;
    if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) { return None; }
    Some((format!("{head}}}"), h))
}

/// A lock file beside the log, for another process writing the same file; removed when dropped. One left behind by a
/// crash is taken over after 10 s; after 2 s of waiting the write goes on without it (the in-process lock still holds).
struct FileLock(Option<PathBuf>);
impl Drop for FileLock { fn drop(&mut self) { if let Some(p) = &self.0 { let _ = std::fs::remove_file(p); } } }
fn file_lock(log: &Path) -> FileLock {
    let p = log.with_extension("lock");
    let t0 = Instant::now();
    loop {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return FileLock(Some(p)),
            Err(_) => {
                let stale = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok()).map(|e| e > Duration::from_secs(10)).unwrap_or(true);
                if stale { let _ = std::fs::remove_file(&p); continue; }
                if t0.elapsed() > Duration::from_secs(2) { return FileLock(None); }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

impl Log {
    pub fn open(path: &Path) -> Log { Log { path: path.to_path_buf(), mx: Mutex::new(()) } }
    pub fn path(&self) -> &Path { &self.path }

    /// The last complete line's (seq, hash), and where a torn tail begins (None when the file ends in a newline).
    fn tail(&self) -> std::io::Result<(u64, String, Option<u64>)> {
        let mut f = match std::fs::File::open(&self.path) { Ok(f) => f, Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, GENESIS.into(), None)), Err(e) => return Err(e) };
        let len = f.metadata()?.len();
        if len == 0 { return Ok((0, GENESIS.into(), None)); }
        let mut from = len.saturating_sub(TAIL);
        loop {
            f.seek(SeekFrom::Start(from))?;
            let mut buf = vec![]; f.read_to_end(&mut buf)?;
            let ends = buf.last() == Some(&b'\n');
            let last_nl = buf.iter().rposition(|b| *b == b'\n');
            let torn = if ends { None } else { Some(from + last_nl.map(|i| i as u64 + 1).unwrap_or(0)) };
            let body = match last_nl { Some(i) => &buf[..i], None => &buf[..0] };
            // the newest complete line that parses (a window that starts mid-line skips its first piece)
            let lines: Vec<&[u8]> = body.split(|b| *b == b'\n').collect();
            let skip_first = from > 0;
            for (i, l) in lines.iter().enumerate().rev() {
                if i == 0 && skip_first { break; }
                if let Ok(v) = serde_json::from_slice::<Value>(l) { if let (Some(s), Some(h)) = (v["seq"].as_u64(), v["hash"].as_str()) { return Ok((s, h.to_string(), torn)); } }
            }
            if from == 0 { return Ok((0, GENESIS.into(), torn)); }
            from = from.saturating_sub(TAIL * 16);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn append(&self, f: &mut std::fs::File, seq: u64, prev: &str, actor: &str, action: &str, target: &str, detail: Value, ip: &str) -> std::io::Result<(Value, String)> {
        let mut m = Map::new();
        m.insert("seq".into(), json!(seq));
        m.insert("at".into(), json!(crate::time::iso_now()));
        m.insert("actor".into(), json!(cut(actor, 64)));
        m.insert("action".into(), json!(cut(action, 64)));
        m.insert("target".into(), json!(cut(target, 128)));
        let d = scrub(&detail);
        m.insert("detail".into(), if d.to_string().len() > 2000 { json!({ "truncated": true }) } else { d });
        if !ip.trim().is_empty() { m.insert("ip".into(), json!(cut(ip.trim(), 64))); }
        m.insert("prev".into(), json!(prev));
        let canonical = serde_json::to_string(&Value::Object(m.clone()))?;
        let hash = chain_hash(prev, &canonical);
        let line = format!("{},\"hash\":\"{hash}\"}}\n", &canonical[..canonical.len() - 1]);
        f.write_all(line.as_bytes())?;
        m.insert("hash".into(), json!(hash));
        Ok((Value::Object(m), hash))
    }

    /// One event at the end of the chain; the entry as written.
    pub fn record(&self, actor: &str, action: &str, target: &str, detail: Value, ip: &str) -> std::io::Result<Value> {
        let _g = self.mx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(d) = self.path.parent() { if !d.exists() { crate::fs::private_dir(d)?; } }
        let _fl = file_lock(&self.path);
        let (mut seq, mut prev, torn) = self.tail()?;
        let fresh = !self.path.exists();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        if fresh { crate::fs::private(&self.path); }
        if let Some(at) = torn {
            // a line a crash cut short: kept aside, cut off, and said
            let len = f.metadata()?.len();
            let mut bytes = vec![];
            { let mut r = std::fs::File::open(&self.path)?; r.seek(SeekFrom::Start(at))?; r.read_to_end(&mut bytes)?; }
            let aside = self.path.with_extension(format!("torn-{}", crate::now_ms()));
            let _ = std::fs::write(&aside, &bytes); crate::fs::private(&aside);
            drop(f);
            std::fs::OpenOptions::new().write(true).open(&self.path)?.set_len(at)?;
            f = std::fs::OpenOptions::new().append(true).open(&self.path)?;
            seq += 1;
            prev = self.append(&mut f, seq, &prev, "", "audit.torn", "", json!({ "bytes": len - at, "keptIn": aside.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }), "")?.1;
        }
        let (v, _) = self.append(&mut f, seq + 1, &prev, actor, action, target, detail, ip)?;
        f.sync_data()?;
        Ok(v)
    }

    fn lines(&self) -> (Vec<String>, bool) {
        let text = match std::fs::read(&self.path) { Ok(b) => String::from_utf8_lossy(&b).into_owned(), Err(_) => return (vec![], false) };
        let torn = !text.is_empty() && !text.ends_with('\n');
        let mut v: Vec<String> = text.split('\n').map(String::from).collect();
        v.pop(); // what follows the last newline: "" or the torn piece
        (v, torn)
    }

    /// Walks the chain from the first line: the first line that does not follow (wrong seq, wrong prev, a hash that is
    /// not its own) is `broken_at`.
    pub fn verify(&self) -> Verified {
        let _g = self.mx.lock().unwrap_or_else(|e| e.into_inner());
        let (lines, torn) = self.lines();
        let mut prev = GENESIS.to_string();
        let mut want = 1u64;
        let broken = |at: u64, why: String, n: u64| Verified { ok: false, broken_at: Some(at), entries: n, torn, why: Some(why) };
        for l in &lines {
            let n = want - 1;
            let Ok(v) = serde_json::from_str::<Value>(l) else { return broken(want, format!("line {want} is not JSON"), n) };
            let seq = v["seq"].as_u64().unwrap_or(0);
            if seq != want { return broken(want, format!("expected seq {want}, found {seq}"), n); }
            if v["prev"].as_str() != Some(prev.as_str()) { return broken(seq, format!("seq {seq} does not follow the line before"), n); }
            let Some((canonical, h)) = split_line(l) else { return broken(seq, format!("seq {seq} has no hash of the expected shape"), n) };
            if chain_hash(&prev, &canonical) != h { return broken(seq, format!("seq {seq} was changed after it was written"), n); }
            prev = h.to_string(); want += 1;
        }
        Verified { ok: true, broken_at: None, entries: want - 1, torn, why: None }
    }

    /// The entries newest first: `since` = only those with a higher seq, `action` = that action or its family
    /// ("signin" also gives "signin.failed"), at most `limit`.
    pub fn read(&self, since: u64, limit: usize, action: &str) -> Vec<Value> {
        let _g = self.mx.lock().unwrap_or_else(|e| e.into_inner());
        let (lines, _) = self.lines();
        let fam = format!("{action}.");
        lines.iter().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["seq"].as_u64().unwrap_or(0) > since)
            .filter(|v| action.is_empty() || v["action"].as_str().map(|a| a == action || a.starts_with(&fam)).unwrap_or(false))
            .take(limit).collect()
    }
    /// The newest entry of that action.
    pub fn last_of(&self, action: &str) -> Option<Value> { self.read(0, usize::MAX, action).into_iter().find(|v| v["action"] == json!(action)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pgm-audit-{name}-{}", crate::crypto::random_hex(6)));
        std::fs::create_dir_all(&d).unwrap();
        d.join("security").join("audit.jsonl")
    }
    #[test]
    fn a_chain_of_events_verifies_and_reads_newest_first() {
        let p = tmp("chain"); let l = Log::open(&p);
        assert_eq!(l.verify(), Verified { ok: true, broken_at: None, entries: 0, torn: false, why: None });
        l.record("alice", "signin.failed", "alice", json!({}), "203.0.113.1").unwrap();
        l.record("alice", "signin", "alice", json!({ "client": "web" }), "203.0.113.1").unwrap();
        let e = l.record("admin", "account.create", "bob", json!({ "admin": false }), "").unwrap();
        assert_eq!(e["seq"], json!(3));
        let v = l.verify(); assert!(v.ok && v.entries == 3 && !v.torn, "{v:?}");
        let all = l.read(0, 10, "");
        assert_eq!(all.iter().map(|x| x["seq"].as_u64().unwrap()).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert_eq!(l.read(0, 10, "signin").len(), 2);
        assert_eq!(l.read(2, 10, "").len(), 1);
        assert!(all[2].get("ip").is_some() && all[0].get("ip").is_none());
        assert_eq!(all[1]["prev"], all[2]["hash"]); assert_eq!(all[2]["prev"], json!(GENESIS));
        let _ = std::fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }
    #[test]
    fn a_changed_or_removed_line_is_found() {
        let p = tmp("tamper"); let l = Log::open(&p);
        for i in 0..5 { l.record("a", "x", &format!("t{i}"), json!({ "n": i }), "").unwrap(); }
        let good = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, good.replacen("\"t2\"", "\"t9\"", 1)).unwrap();
        let v = l.verify(); assert!(!v.ok && v.broken_at == Some(3), "{v:?}");
        // a line removed in the middle
        let mut ls: Vec<&str> = good.lines().collect(); ls.remove(1);
        std::fs::write(&p, ls.join("\n") + "\n").unwrap();
        let v = l.verify(); assert!(!v.ok && v.broken_at == Some(2), "{v:?}");
        // a forged line with a fresh hash but the wrong prev
        std::fs::write(&p, &good).unwrap(); assert!(l.verify().ok);
        let mut ls: Vec<String> = good.lines().map(String::from).collect();
        let c = chain_hash(GENESIS, "{\"seq\":4}"); ls[3] = format!("{{\"seq\":4,\"prev\":\"{GENESIS}\",\"hash\":\"{c}\"}}");
        std::fs::write(&p, ls.join("\n") + "\n").unwrap();
        assert_eq!(l.verify().broken_at, Some(4));
        let _ = std::fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }
    #[test]
    fn a_torn_last_line_is_reported_then_moved_aside() {
        let p = tmp("torn"); let l = Log::open(&p);
        l.record("a", "x", "", json!({}), "").unwrap(); l.record("a", "y", "", json!({}), "").unwrap();
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"seq\":3,\"at\":\"20").unwrap();
        let v = l.verify(); assert!(v.ok && v.torn && v.entries == 2, "{v:?}");
        let e = l.record("a", "z", "", json!({}), "").unwrap();
        assert_eq!(e["seq"], json!(4));
        let v = l.verify(); assert!(v.ok && !v.torn && v.entries == 4, "{v:?}");
        assert_eq!(l.read(0, 10, "audit.torn").len(), 1);
        assert!(std::fs::read_dir(p.parent().unwrap()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("torn-")));
        let _ = std::fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }
    #[test]
    fn secrets_never_go_in() {
        let p = tmp("secret"); let l = Log::open(&p);
        l.record("a", "account.password", "b", json!({ "password": "hunter2-PW", "newToken": "tok-SECRET", "apiKey": "k-SECRET", "nested": { "cookie": "c-SECRET", "ok": 1 }, "keepMachines": true }), "").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(!t.contains("hunter2") && !t.contains("SECRET") && t.contains("keepMachines") && t.contains("\"ok\":1"), "{t}");
        assert!(l.verify().ok);
        let _ = std::fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }
    #[test]
    fn writers_at_once_keep_one_chain() {
        let p = tmp("race"); let l = std::sync::Arc::new(Log::open(&p));
        let hs: Vec<_> = (0..8).map(|i| { let l = l.clone(); std::thread::spawn(move || for j in 0..10 { l.record("a", "x", &format!("{i}-{j}"), json!({}), "").unwrap(); }) }).collect();
        for h in hs { h.join().unwrap(); }
        // a second Log on the same file (another process) goes through the lock file
        let l2 = Log::open(&p); l2.record("b", "x", "", json!({}), "").unwrap();
        let v = l.verify(); assert!(v.ok && v.entries == 81, "{v:?}");
        assert!(!p.with_extension("lock").exists());
        let _ = std::fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }
}
