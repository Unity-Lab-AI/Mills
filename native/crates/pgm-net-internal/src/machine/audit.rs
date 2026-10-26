//! The machine's audit log (spec 7.1, R331): `<state>/machine-audit.jsonl`, append-only, one JSON line per call,
//! hash-chained so an edited, removed or reordered line shows. File contents are never logged: a line carries the
//! path as given, the decision and reason, a byte count and an optional sha256 **digest** of the contents.
//!
//! - Each line: `{seq, prev, t, actor, op, path, decision, reason, digest?, bytes?, hash}` where
//!   `hash = sha256(the line without "hash")` and `prev` is the previous line's `hash` (the first line of a fresh
//!   log has the all-zero `prev`).
//! - Rotation at `max_bytes`: `log` becomes `log.1`, `log.1` becomes `log.2` and so on (the newest is `.1`); the
//!   chain carries over, the first line of the new file has the last hash of the old one as `prev`.
//! - A sidecar `log.head` holds `{seq, hash}` of the last line, so a removed **tail** shows too (an attacker who can
//!   write both files can rewrite both; both are safety-floor paths, no tool can touch them).
//! - Retention ([`AuditLog::prune`]) drops rotated files older than the keep time; the oldest file left anchors the
//!   chain at its first line's `prev`.
//! - The hub's copy ([`HubRecord`]) has the same fields minus the digest and the byte count; the owner reads it, an
//!   admin does not ([`may_read_hub_copy`]; the hub routes call it, they must not read the file for anyone else).

use pgm_core_internal::crypto::sha256_hex;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const ZERO_HASH: &str = concat!("00000000", "00000000", "00000000", "00000000", "00000000", "00000000", "00000000", "00000000");
/// Rotated at 50 MB (7.1).
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Rotated files kept (with 90 days of retention in [`AuditLog::prune`]).
pub const DEFAULT_KEEP_FILES: usize = 20;

pub fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// sha256 of some contents, for [`Event::digest`] (the contents themselves are never written).
pub fn digest_of(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    Prompt,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Allow => "allow",
            Verdict::Deny => "deny",
            Verdict::Prompt => "prompt",
        }
    }
}

/// What the caller reports for one call.
#[derive(Clone, Debug)]
pub struct Event {
    pub t: i64,
    /// `account/session/run` as the request carried it (or `machine-ui`, `terminal`, `hub-narrowing`).
    pub actor: String,
    pub op: String,
    /// The path or command as given (and, when it differs, as canonicalised, joined by ` -> `).
    pub path: String,
    pub decision: Verdict,
    pub reason: String,
    pub digest: Option<String>,
    pub bytes: Option<u64>,
}

impl Event {
    pub fn new(t: i64, actor: &str, op: &str, path: &str, decision: Verdict, reason: &str) -> Event {
        Event { t, actor: actor.into(), op: op.into(), path: path.into(), decision, reason: reason.into(), digest: None, bytes: None }
    }
    /// From a scope decision.
    pub fn from_decision(t: i64, actor: &str, op: &str, path: &str, d: &super::scope::Decision) -> Event {
        Event::new(t, actor, op, path, if d.allow { Verdict::Allow } else { Verdict::Deny }, &d.reason)
    }
    pub fn with_contents(mut self, contents: &[u8]) -> Event {
        self.digest = Some(digest_of(contents));
        self.bytes = Some(contents.len() as u64);
        self
    }
    pub fn with_bytes(mut self, n: u64) -> Event {
        self.bytes = Some(n);
        self
    }
}

/// One line as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub seq: u64,
    pub prev: String,
    pub t: i64,
    pub actor: String,
    pub op: String,
    pub path: String,
    pub decision: String,
    pub reason: String,
    pub digest: Option<String>,
    pub bytes: Option<u64>,
    pub hash: String,
}

impl Record {
    fn body(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("seq".into(), json!(self.seq));
        m.insert("prev".into(), json!(self.prev));
        m.insert("t".into(), json!(self.t));
        m.insert("actor".into(), json!(self.actor));
        m.insert("op".into(), json!(self.op));
        m.insert("path".into(), json!(self.path));
        m.insert("decision".into(), json!(self.decision));
        m.insert("reason".into(), json!(self.reason));
        if let Some(d) = &self.digest {
            m.insert("digest".into(), json!(d));
        }
        if let Some(b) = self.bytes {
            m.insert("bytes".into(), json!(b));
        }
        m
    }
    pub fn to_line(&self) -> String {
        let mut m = self.body();
        m.insert("hash".into(), json!(self.hash));
        Value::Object(m).to_string()
    }
    fn compute_hash(&self) -> String {
        sha256_hex(Value::Object(self.body()).to_string().as_bytes())
    }
    pub fn parse(line: &str) -> Result<Record, String> {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
        let o = v.as_object().ok_or("not an object")?;
        let s = |k: &str| o.get(k).and_then(|x| x.as_str()).map(|x| x.to_string()).ok_or(format!("missing {k}"));
        Ok(Record {
            seq: o.get("seq").and_then(|x| x.as_u64()).ok_or("missing seq")?,
            prev: s("prev")?,
            t: o.get("t").and_then(|x| x.as_i64()).ok_or("missing t")?,
            actor: s("actor")?,
            op: s("op")?,
            path: s("path")?,
            decision: s("decision")?,
            reason: s("reason")?,
            digest: o.get("digest").and_then(|x| x.as_str()).map(|x| x.to_string()),
            bytes: o.get("bytes").and_then(|x| x.as_u64()),
            hash: s("hash")?,
        })
    }
}

/// What [`AuditLog::verify`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verify {
    pub ok: bool,
    pub lines: u64,
    pub files: usize,
    /// `(file, line number from 1, why)` of the first problem.
    pub error: Option<(String, u64, String)>,
}

struct State {
    seq: u64,
    last: String,
}

pub struct AuditLog {
    path: PathBuf,
    max_bytes: u64,
    keep_files: usize,
    st: Mutex<State>,
}

fn rotated(path: &Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

fn head_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".head");
    PathBuf::from(s)
}

fn anchor_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".anchor");
    PathBuf::from(s)
}

fn last_record(file: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines().rev().find(|l| !l.trim().is_empty()).and_then(|l| Record::parse(l).ok())
}

impl AuditLog {
    /// Open (creating the folder) and find the end of the chain.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<AuditLog> {
        AuditLog::open_with(path, DEFAULT_MAX_BYTES, DEFAULT_KEEP_FILES)
    }

    pub fn open_with(path: impl AsRef<Path>, max_bytes: u64, keep_files: usize) -> std::io::Result<AuditLog> {
        let path = path.as_ref().to_path_buf();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let (seq, last) = match last_record(&path).or_else(|| last_record(&rotated(&path, 1))) {
            Some(r) => (r.seq + 1, r.hash),
            None => match read_head(&path) {
                // the files are gone but the head is not: carry on from the head (verify will show the gap)
                Some((s, h)) => (s + 1, h),
                None => (0, ZERO_HASH.to_string()),
            },
        };
        Ok(AuditLog { path, max_bytes, keep_files: keep_files.max(1), st: Mutex::new(State { seq, last }) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line (denials too). Returns the record written.
    pub fn append(&self, e: Event) -> std::io::Result<Record> {
        let mut st = self.st.lock().unwrap_or_else(|p| p.into_inner());
        self.rotate_if_needed()?;
        let mut r = Record {
            seq: st.seq,
            prev: st.last.clone(),
            t: e.t,
            actor: e.actor,
            op: e.op,
            path: e.path,
            decision: e.decision.as_str().into(),
            reason: e.reason,
            digest: e.digest,
            bytes: e.bytes,
            hash: String::new(),
        };
        r.hash = r.compute_hash();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(format!("{}\n", r.to_line()).as_bytes())?;
        f.flush()?;
        let _ = std::fs::write(head_path(&self.path), json!({"seq": r.seq, "hash": r.hash}).to_string());
        st.seq += 1;
        st.last = r.hash.clone();
        Ok(r)
    }

    fn rotate_if_needed(&self) -> std::io::Result<()> {
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size < self.max_bytes || size == 0 {
            return Ok(());
        }
        let _ = std::fs::remove_file(rotated(&self.path, self.keep_files));
        for n in (1..self.keep_files).rev() {
            let from = rotated(&self.path, n);
            if from.exists() {
                std::fs::rename(&from, rotated(&self.path, n + 1))?;
            }
        }
        std::fs::rename(&self.path, rotated(&self.path, 1))?;
        self.write_anchor();
        Ok(())
    }

    /// Remember where the oldest file left starts (after a rotation or a prune dropped older ones), so removing
    /// its first line is still seen.
    fn write_anchor(&self) {
        let first = self.files().first().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| t.lines().next().and_then(|l| Record::parse(l).ok()));
        if let Some(r) = first {
            let _ = std::fs::write(anchor_path(&self.path), json!({"seq": r.seq, "prev": r.prev}).to_string());
        }
    }

    /// Delete rotated files whose last write is older than `keep_secs` (default 90 days in the app). The active
    /// file is never deleted. Returns how many went.
    pub fn prune(&self, now: i64, keep_secs: i64) -> usize {
        let mut n = 0;
        for i in 1..=self.keep_files {
            let p = rotated(&self.path, i);
            let age = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
            if let Some(m) = age {
                if now - m > keep_secs && std::fs::remove_file(&p).is_ok() {
                    n += 1;
                }
            }
        }
        if n > 0 {
            self.write_anchor();
        }
        n
    }

    /// Files oldest first (rotated `.N` ... `.1`, then the active one), those that exist.
    fn files(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = (1..=self.keep_files).rev().map(|n| rotated(&self.path, n)).filter(|p| p.exists()).collect();
        if self.path.exists() {
            v.push(self.path.clone());
        }
        v
    }

    /// Walk every file: each line must parse, carry the next `seq`, have the previous line's `hash` as `prev` and the
    /// hash of its own content; the last line must match the head sidecar.
    pub fn verify(&self) -> Verify {
        let _g = self.st.lock().unwrap_or_else(|p| p.into_inner());
        let files = self.files();
        let mut lines = 0u64;
        let mut prev: Option<(u64, String)> = None;
        let fail = |file: &Path, n: u64, why: &str, lines: u64, files: usize| Verify { ok: false, lines, files, error: Some((file.display().to_string(), n, why.to_string())) };
        for f in &files {
            let text = match std::fs::read_to_string(f) {
                Ok(t) => t,
                Err(e) => return fail(f, 0, &format!("unreadable: {e}"), lines, files.len()),
            };
            for (i, line) in text.lines().enumerate() {
                let n = i as u64 + 1;
                let r = match Record::parse(line) {
                    Ok(r) => r,
                    Err(e) => return fail(f, n, &format!("bad line: {e}"), lines, files.len()),
                };
                if r.compute_hash() != r.hash {
                    return fail(f, n, "edited line (its hash does not match)", lines, files.len());
                }
                match &prev {
                    Some((s, h)) => {
                        if r.seq != s + 1 {
                            return fail(f, n, "a line is missing or out of order (seq)", lines, files.len());
                        }
                        if &r.prev != h {
                            return fail(f, n, "chain broken (prev does not match the line before)", lines, files.len());
                        }
                    }
                    None => {
                        // the oldest file left: its first line anchors the chain; a fresh log starts at 0
                        let want = std::fs::read_to_string(anchor_path(&self.path)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| (v["seq"].as_u64().unwrap_or(u64::MAX), v["prev"].as_str().unwrap_or("").to_string())).unwrap_or((0, ZERO_HASH.to_string()));
                        if (r.seq, r.prev.clone()) != want {
                            return fail(f, n, "the first line is not where the log starts (lines removed from the start)", lines, files.len());
                        }
                    }
                }
                prev = Some((r.seq, r.hash));
                lines += 1;
            }
        }
        if let Some((s, h)) = read_head(&self.path) {
            match &prev {
                Some((ls, lh)) if *ls == s && *lh == h => {}
                _ => {
                    return Verify { ok: false, lines, files: files.len(), error: Some((head_path(&self.path).display().to_string(), 0, "the end of the log does not match the head (lines removed from the end)".into())) }
                }
            }
        }
        Verify { ok: true, lines, files: files.len(), error: None }
    }

    /// Every record, oldest first (for the machine's own "Audit" screen; no tool offers this).
    pub fn records(&self) -> Vec<Record> {
        self.files().iter().filter_map(|f| std::fs::read_to_string(f).ok()).flat_map(|t| t.lines().filter_map(|l| Record::parse(l).ok()).collect::<Vec<_>>()).collect()
    }
}

fn read_head(path: &Path) -> Option<(u64, String)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(head_path(path)).ok()?).ok()?;
    Some((v.get("seq")?.as_u64()?, v.get("hash")?.as_str()?.to_string()))
}

// ------------------------------------------------------------------ the hub's copy

/// A line as the hub keeps it, `<profile>/machine-audit.jsonl` per account: the same fields minus the digest and
/// the byte count (the hub sees paths, tool and decision, never contents), plus the machine id and the account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubRecord {
    pub account: String,
    pub machine: String,
    pub seq: u64,
    pub t: i64,
    pub actor: String,
    pub op: String,
    pub path: String,
    pub decision: String,
    pub reason: String,
    /// The machine line's hash, so the owner can compare the copy with the machine's log.
    pub hash: String,
}

impl HubRecord {
    pub fn from_record(account: &str, machine: &str, r: &Record) -> HubRecord {
        HubRecord {
            account: account.into(),
            machine: machine.into(),
            seq: r.seq,
            t: r.t,
            actor: r.actor.clone(),
            op: r.op.clone(),
            path: r.path.clone(),
            decision: r.decision.clone(),
            reason: r.reason.clone(),
            hash: r.hash.clone(),
        }
    }
    pub fn to_line(&self) -> String {
        json!({"account": self.account, "machine": self.machine, "seq": self.seq, "t": self.t, "actor": self.actor, "op": self.op, "path": self.path, "decision": self.decision, "reason": self.reason, "hash": self.hash}).to_string()
    }
    pub fn parse(line: &str) -> Option<HubRecord> {
        let v: Value = serde_json::from_str(line).ok()?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
        Some(HubRecord { account: s("account")?, machine: s("machine")?, seq: v.get("seq")?.as_u64()?, t: v.get("t")?.as_i64()?, actor: s("actor")?, op: s("op")?, path: s("path")?, decision: s("decision")?, reason: s("reason")?, hash: s("hash")? })
    }
}

/// The only rule for the hub's copy: its **owner** reads it. An admin does not (7.1), whatever the admin flag says.
/// The `/api/machines/<id>/audit` route must call this before it opens the file (that wiring is the hub routes').
pub fn may_read_hub_copy(owner: &str, requester: &str, _requester_is_admin: bool) -> bool {
    !owner.is_empty() && owner == requester
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::scope::Decision;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pgm-audit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn ev(i: i64) -> Event {
        Event::new(1000 + i, "sponge/s1/r1", "read", &format!("/home/a/f{i}.txt"), if i % 2 == 0 { Verdict::Allow } else { Verdict::Deny }, "because")
    }

    #[test]
    fn r331_audit_every_call_one_line_and_denials_too() {
        let d = tmp("lines");
        let log = AuditLog::open(d.join("machine-audit.jsonl")).unwrap();
        let allow = Decision { allow: true, reason: "read allowed".into() };
        let deny = Decision { allow: false, reason: "safety floor".into() };
        log.append(Event::from_decision(1, "a/s/r", "read", "/x", &allow)).unwrap();
        log.append(Event::from_decision(2, "a/s/r", "read", "/state/token", &deny)).unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(text.lines().count(), 2);
        let recs = log.records();
        assert_eq!(recs[0].decision, "allow");
        assert_eq!(recs[1].decision, "deny");
        assert_eq!(recs[0].prev, ZERO_HASH);
        assert_eq!(recs[1].prev, recs[0].hash);
        assert!(log.verify().ok);
    }

    #[test]
    fn r331_contents_are_never_logged_only_a_digest() {
        let d = tmp("contents");
        let log = AuditLog::open(d.join("a.jsonl")).unwrap();
        let secret = b"TOP-SECRET-FILE-CONTENTS";
        log.append(ev(0).with_contents(secret)).unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert!(!text.contains("TOP-SECRET"));
        assert!(text.contains(&digest_of(secret)));
        assert!(text.contains("\"bytes\":24"));
        // the API has no field for contents: Event holds only a digest
        let r = &log.records()[0];
        assert_eq!(r.digest.as_deref(), Some(digest_of(secret).as_str()));
    }

    #[test]
    fn r331_chain_breaks_on_edit_removal_reorder_and_tail_cut() {
        let d = tmp("chain");
        let p = d.join("a.jsonl");
        let log = AuditLog::open(&p).unwrap();
        for i in 0..6 {
            log.append(ev(i)).unwrap();
        }
        assert_eq!(log.verify(), Verify { ok: true, lines: 6, files: 1, error: None });
        let orig = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = orig.lines().collect();
        let put = |v: Vec<String>| std::fs::write(&p, v.join("\n") + "\n").unwrap();
        // edit: a deny turned into an allow
        put(lines.iter().enumerate().map(|(i, l)| if i == 3 { l.replace("\"deny\"", "\"allow\"") } else { l.to_string() }).collect());
        let v = log.verify();
        assert!(!v.ok && v.error.as_ref().unwrap().1 == 4, "{v:?}");
        // edit that recomputes its own hash still breaks the next prev
        let mut r = Record::parse(lines[2]).unwrap();
        r.reason = "changed".into();
        r.hash = r.compute_hash();
        put(lines.iter().enumerate().map(|(i, l)| if i == 2 { r.to_line() } else { l.to_string() }).collect());
        let v = log.verify();
        assert!(!v.ok && v.error.as_ref().unwrap().2.contains("chain"), "{v:?}");
        // removal in the middle
        put(lines.iter().enumerate().filter(|(i, _)| *i != 2).map(|(_, l)| l.to_string()).collect());
        let v = log.verify();
        assert!(!v.ok && v.error.as_ref().unwrap().2.contains("missing"), "{v:?}");
        // reorder
        let mut re: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        re.swap(1, 2);
        put(re);
        assert!(!log.verify().ok);
        // first line removed
        put(lines[1..].iter().map(|l| l.to_string()).collect());
        let v = log.verify();
        assert!(!v.ok && v.error.as_ref().unwrap().2.contains("start"), "{v:?}");
        // tail cut
        put(lines[..4].iter().map(|l| l.to_string()).collect());
        let v = log.verify();
        assert!(!v.ok && v.error.as_ref().unwrap().2.contains("end of the log"), "{v:?}");
        // restored: fine again
        put(lines.iter().map(|l| l.to_string()).collect());
        assert!(log.verify().ok);
        // garbage line
        std::fs::write(&p, orig.clone() + "not json\n").unwrap();
        assert!(!log.verify().ok);
    }

    #[test]
    fn r331_rotation_keeps_the_chain_across_files_and_reopen_continues() {
        let d = tmp("rot");
        let p = d.join("a.jsonl");
        let log = AuditLog::open_with(&p, 600, 4).unwrap();
        for i in 0..20 {
            log.append(ev(i)).unwrap();
        }
        let v = log.verify();
        assert!(v.ok, "{v:?}");
        assert!(v.files > 1 && v.files <= 5, "{v:?}");
        assert!(rotated(&p, 1).exists());
        let first_of_new = std::fs::read_to_string(&p).unwrap().lines().next().map(|l| Record::parse(l).unwrap()).unwrap();
        let last_of_old = last_record(&rotated(&p, 1)).unwrap();
        assert_eq!(first_of_new.prev, last_of_old.hash, "the chain crosses the rotation");
        // a reopened log (a restart) carries on
        let n = log.records().len();
        drop(log);
        let log2 = AuditLog::open_with(&p, 600, 4).unwrap();
        log2.append(ev(99)).unwrap();
        assert!(log2.verify().ok);
        assert_eq!(log2.records().len(), n + 1);
        // an emptied middle file (a rotated file removed) breaks it
        if rotated(&p, 2).exists() {
            std::fs::remove_file(rotated(&p, 2)).unwrap();
            assert!(!log2.verify().ok);
        }
    }

    #[test]
    fn r331_prune_keeps_the_active_file() {
        let d = tmp("prune");
        let p = d.join("a.jsonl");
        let log = AuditLog::open_with(&p, 300, 3).unwrap();
        for i in 0..12 {
            log.append(ev(i)).unwrap();
        }
        let before = log.files().len();
        assert!(before > 1);
        let gone = log.prune(unix_now() + 100 * 86400, 90 * 86400);
        assert!(gone >= 1);
        assert!(p.exists());
        assert!(log.verify().ok, "the oldest file left anchors the chain");
    }

    #[test]
    fn r331_hub_copy_has_no_digest_and_only_the_owner_reads_it() {
        let d = tmp("hub");
        let log = AuditLog::open(d.join("a.jsonl")).unwrap();
        let r = log.append(ev(0).with_contents(b"abc")).unwrap();
        let h = HubRecord::from_record("sponge", "m1", &r);
        let line = h.to_line();
        assert!(!line.contains("digest") && !line.contains("bytes"));
        assert_eq!(HubRecord::parse(&line), Some(h));
        assert!(line.contains(&r.hash));
        assert!(may_read_hub_copy("sponge", "sponge", false));
        assert!(!may_read_hub_copy("sponge", "gee", false));
        assert!(!may_read_hub_copy("sponge", "admin", true), "an admin does not (403 in the route)");
        assert!(!may_read_hub_copy("", "", true));
    }
}
