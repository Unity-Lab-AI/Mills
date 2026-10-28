//! A copy's journal of its own writes: one JSON record per line, add-only (`<dir>/journal.jsonl`). Each record has the
//! next `seq`; a peer asks for everything after the last seq it has. When the file grows past a limit it is rewritten
//! with only the newest record of each (kind, id) (their seqs unchanged), so a peer that is far behind still gets every
//! record's latest value.
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::record::Record;

struct Entry { seq: u64, off: u64, len: u64, kind: String, id: String }
pub struct Journal { file: PathBuf, entries: Vec<Entry>, last: u64, size: u64, limit: u64 }

impl Journal {
    /// Opens (or starts) the journal at `file`. A torn last line (a crash mid-write) is cut off.
    pub fn open(file: &Path) -> Journal {
        let mut j = Journal { file: file.to_path_buf(), entries: vec![], last: 0, size: 0, limit: 256 << 20 };
        if let Ok(f) = std::fs::File::open(file) {
            let mut r = std::io::BufReader::new(f);
            let mut off = 0u64; let mut line = String::new();
            loop {
                line.clear();
                let n = match r.read_line(&mut line) { Ok(0) | Err(_) => break, Ok(n) => n as u64 };
                if !line.ends_with('\n') { break; } // torn
                if let Ok(v) = serde_json::from_str::<Value>(line.trim_end()) {
                    if let (Some(seq), Some(k), Some(id)) = (v["seq"].as_u64(), v["kind"].as_str(), v["id"].as_str()) {
                        if seq > j.last { j.entries.push(Entry { seq, off, len: n, kind: k.into(), id: id.into() }); j.last = seq; }
                    }
                }
                off += n;
            }
            j.size = off;
            if std::fs::metadata(file).map(|m| m.len()).unwrap_or(0) > off { if let Ok(f) = std::fs::OpenOptions::new().write(true).open(file) { let _ = f.set_len(off); } }
        }
        j
    }
    /// Size limit before a compaction (tests set it small).
    pub fn set_limit(&mut self, bytes: u64) { self.limit = bytes; }
    pub fn last_seq(&self) -> u64 { self.last }
    pub fn next_seq(&self) -> u64 { self.last + 1 }
    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
    /// Adds a record (its seq must be the next one).
    pub fn append(&mut self, r: &Record) -> std::io::Result<()> {
        let line = format!("{}\n", r.to_value());
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.file)?;
        f.write_all(line.as_bytes())?;
        f.flush()?;
        self.entries.push(Entry { seq: r.seq, off: self.size, len: line.len() as u64, kind: r.kind.clone(), id: r.id.clone() });
        self.size += line.len() as u64; self.last = self.last.max(r.seq);
        if self.size > self.limit { self.compact(); }
        Ok(())
    }
    fn first_after(&self, seq: u64) -> usize { self.entries.partition_point(|e| e.seq <= seq) }
    /// How many records come after `seq`.
    pub fn count_after(&self, seq: u64) -> usize { self.entries.len() - self.first_after(seq) }
    /// The records after `seq`, in order, up to about `max_bytes` (at least one) and `max_n`.
    pub fn after(&self, seq: u64, max_bytes: u64, max_n: usize) -> Vec<Value> {
        let mut out = vec![]; let mut bytes = 0u64;
        let Ok(mut f) = std::fs::File::open(&self.file) else { return out };
        for e in &self.entries[self.first_after(seq)..] {
            if out.len() >= max_n || (!out.is_empty() && bytes + e.len > max_bytes) { break; }
            let mut buf = vec![0u8; e.len as usize];
            if f.seek(SeekFrom::Start(e.off)).is_err() || f.read_exact(&mut buf).is_err() { break; }
            match serde_json::from_slice::<Value>(&buf) { Ok(v) => out.push(v), Err(_) => continue }
            bytes += e.len;
        }
        out
    }
    /// Keeps only the newest record of each (kind, id); seqs stay as they were.
    pub fn compact(&mut self) {
        let mut newest: HashMap<(&str, &str), u64> = HashMap::new();
        for e in &self.entries { newest.insert((e.kind.as_str(), e.id.as_str()), e.seq); }
        let keep: Vec<&Entry> = self.entries.iter().filter(|e| newest.get(&(e.kind.as_str(), e.id.as_str())) == Some(&e.seq)).collect();
        if keep.len() == self.entries.len() { self.limit = self.limit.max(self.size * 2); return; }
        let Ok(mut f) = std::fs::File::open(&self.file) else { return };
        let tmp = self.file.with_extension("jsonl.tmp");
        let Ok(mut out) = std::fs::File::create(&tmp) else { return };
        let mut fresh = vec![]; let mut off = 0u64;
        for e in keep {
            let mut buf = vec![0u8; e.len as usize];
            if f.seek(SeekFrom::Start(e.off)).is_err() || f.read_exact(&mut buf).is_err() || out.write_all(&buf).is_err() { let _ = std::fs::remove_file(&tmp); return; }
            fresh.push(Entry { seq: e.seq, off, len: e.len, kind: e.kind.clone(), id: e.id.clone() }); off += e.len;
        }
        if out.flush().is_err() || std::fs::rename(&tmp, &self.file).is_err() { let _ = std::fs::remove_file(&tmp); return; }
        self.entries = fresh; self.size = off;
        if self.size * 2 > self.limit { self.limit = self.size * 2; } // mostly distinct records: room before the next try
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn rec(seq: u64, id: &str, x: i64) -> Record { Record { kind: "k".into(), id: id.into(), rev: x, time: 0, origin: "h".into(), origin_name: String::new(), seq, value: Some(json!({ "x": x })) } }
    #[test]
    fn records_come_back_after_a_seq_survive_a_reopen_and_compaction_keeps_the_newest() {
        let dir = std::env::temp_dir().join(format!("pgm-sync-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("journal.jsonl");
        let mut j = Journal::open(&f);
        for (i, id) in ["a", "b", "a", "c", "a"].iter().enumerate() { j.append(&rec(i as u64 + 1, id, i as i64)).unwrap(); }
        assert_eq!(j.after(2, 1 << 20, 100).iter().map(|v| v["seq"].as_u64().unwrap()).collect::<Vec<_>>(), vec![3, 4, 5]);
        assert_eq!(j.after(0, 1, 100).len(), 1, "at least one record even over the byte limit");
        // a torn line at the end is cut off on reopen
        std::fs::OpenOptions::new().append(true).open(&f).unwrap().write_all(b"{\"seq\":6,\"ki").unwrap();
        let mut j = Journal::open(&f);
        assert_eq!((j.last_seq(), j.len()), (5, 5));
        j.compact();
        assert_eq!(j.after(0, 1 << 20, 100).iter().map(|v| (v["id"].as_str().unwrap().to_string(), v["seq"].as_u64().unwrap())).collect::<Vec<_>>(), vec![("b".into(), 2), ("c".into(), 4), ("a".into(), 5)]);
        j.append(&rec(6, "b", 9)).unwrap();
        let j = Journal::open(&f);
        assert_eq!((j.last_seq(), j.count_after(4)), (6, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
