//! The engine: one copy's side of the sync. It knows the program's kinds through [`Kind`] adapters, notices the copy's
//! own writes (a scan compares each record with what it last journaled), journals them, applies the peer's records with
//! the kind's rules, and remembers per record what the peer is known to have (so a true conflict, both sides changed it
//! since they last agreed, is told from a plain update).
//!
//! Files in its folder (`dir`): `state.json` (this copy's id, how far it has each origin's journal), `journal.jsonl`
//! (its own writes, add-only), `index.json` (per record: the hash it last journaled or applied, the hash the peer has,
//! its order), `held.jsonl` (records of kinds this build does not know: kept, applied once a newer build knows them).
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::canon::hash;
use crate::journal::Journal;
use crate::record::Record;

/// A true conflict: both copies changed one record since they last agreed. The winner is what stays; the loser must not
/// be thrown away (the kind keeps it in the record's history / activity, or as a copy). `None` = a delete.
/// `winner_from` / `loser_from` are the names of the copies that made them ("live", "dev").
pub struct Conflict<'a> { pub winner: Option<&'a Value>, pub loser: Option<&'a Value>, pub winner_from: &'a str, pub loser_from: &'a str }

/// One kind of record of the program (board tasks, notes, chat messages …): how to read them, check them, store them.
pub trait Kind: Send + Sync {
    /// The kind's name on the wire ("board.task"): lowercase letters, digits, `.`, `-`, `_`.
    fn name(&self) -> &str;
    /// A number that moves when the kind's data may have changed; the engine scans the kind when it moved (and every
    /// so often anyway). None = no such number: scanned every few seconds.
    fn version(&self) -> Option<u64> { None }
    /// Every record now: (id, value).
    fn snapshot(&self) -> Vec<(String, Value)>;
    /// Only the records changed since version `since` (None = not supported: the whole snapshot is read).
    fn changed(&self, _since: u64) -> Option<Vec<(String, Value)>> { None }
    /// Whether a record missing from the snapshot was deleted (a tombstone is journaled). False: records are never
    /// deleted this way (append-only kinds, or deletes kept inside the value).
    fn deletes(&self) -> bool { false }
    /// One record now.
    fn get(&self, id: &str) -> Option<Value>;
    /// The kind's shape check: a record that fails it is refused and logged, never applied.
    fn check(&self, id: &str, v: &Value) -> Result<(), String>;
    /// (rev, time) of a value: higher wins. None = (0, when the engine saw the change).
    fn stamp(&self, _v: &Value) -> Option<(i64, i64)> { None }
    /// What makes a record the same thing on both copies (for kinds whose ids each copy hands out: two different tasks
    /// may get the same id). None = the id is enough.
    fn identity(&self, _v: &Value) -> Option<String> { None }
    /// Moves this copy's record `id` to a new id of its own (it lost the id to the peer's record); the new id.
    fn rekey(&self, _id: &str) -> Option<String> { None }
    /// A kind with its own merge (append-only maxima, sets): the merged value, or None for newest-wins.
    fn merge(&self, _id: &str, _local: Option<&Value>, _incoming: &Value) -> Option<Value> { None }
    /// Stores a value (creating or replacing the record); the value as stored.
    fn put(&self, id: &str, v: &Value) -> Result<Value, String>;
    /// Deletes a record (a tombstone came in).
    fn remove(&self, _id: &str) -> Result<(), String> { Ok(()) }
    /// Keeps the losing side of a true conflict.
    fn keep_loser(&self, _id: &str, _c: &Conflict) {}
}

#[derive(Clone, Default, Debug, PartialEq)]
struct Entry { mine: Option<String>, peer: Option<String>, rev: i64, time: i64 }

/// How much a scan looks at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scan {
    /// kinds whose version moved
    Changed,
    /// those, and the kinds with no version
    Periodic,
    /// every kind, whole snapshot (deletes noticed)
    Full,
}

/// What became of a batch of the peer's records.
#[derive(Default, Debug, Clone)]
pub struct Report {
    /// the last seq of the sender's journal this copy now has
    pub have: u64,
    pub applied: usize,
    /// already the same here
    pub same: usize,
    /// this copy's value won a conflict (seqs)
    pub kept: Vec<u64>,
    /// set aside: the record lost its id to one here, its origin moves it to a new id (seqs)
    pub deferred: Vec<u64>,
    /// refused: (seq, kind, id, why)
    pub refused: Vec<(u64, String, String, String)>,
    /// of kinds this build does not know: kept in held.jsonl
    pub held: usize,
}
impl Report {
    pub fn to_value(&self) -> Value {
        json!({ "ok": true, "have": self.have, "applied": self.applied, "same": self.same, "kept": self.kept, "deferred": self.deferred, "held": self.held,
            "refused": self.refused.iter().map(|(s, k, i, w)| json!({ "seq": s, "kind": k, "id": i, "why": w })).collect::<Vec<_>>() })
    }
}

#[derive(Default, Clone)]
struct Peer { up: bool, hub: String, name: String, have: u64, last_out: i64, error: String }
#[derive(Default, Clone)]
struct Stats { applied: u64, refused: u64, conflicts: u64, deferred: u64, held: u64, last_in: i64 }

struct Inner {
    journal: Journal,
    index: HashMap<String, HashMap<String, Entry>>,
    have: HashMap<String, u64>,
    versions: HashMap<String, u64>,
    dirty: bool,
    peer: Peer,
    stats: Stats,
}

pub struct Engine {
    dir: PathBuf,
    id: String,
    name: String,
    kinds: Vec<Arc<dyn Kind>>,
    inner: Mutex<Inner>,
    /// woken whenever the journal grows (the sender waits on it)
    pub grew: tokio::sync::Notify,
    /// log lines start with this ("[vr-hub] sync:")
    tag: String,
}

fn now() -> i64 { pgm_core_internal::now_ms() as i64 }

impl Engine {
    /// Opens this copy's side in `dir` (made if missing). `name` = what this copy is called ("live", "dev").
    /// Held records whose kind is now known are applied straight away.
    pub fn open(dir: &Path, name: &str, kinds: Vec<Arc<dyn Kind>>, tag: &str) -> Engine {
        let _ = std::fs::create_dir_all(dir);
        let _ = pgm_core_internal::fs::private_dir(dir);
        let st = pgm_core_internal::json::read_json(&dir.join("state.json")).unwrap_or(Value::Null);
        let id = st["hub"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| format!("h{}", pgm_core_internal::crypto::random_hex(8)));
        let have: HashMap<String, u64> = st["have"].as_object().map(|m| m.iter().filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n))).collect()).unwrap_or_default();
        let peer = Peer { hub: st["peer"]["hub"].as_str().unwrap_or("").into(), name: st["peer"]["name"].as_str().unwrap_or("").into(), ..Default::default() };
        let mut index: HashMap<String, HashMap<String, Entry>> = HashMap::new();
        if let Some(m) = pgm_core_internal::json::read_json(&dir.join("index.json")).as_ref().and_then(Value::as_object) {
            for (k, ids) in m {
                let e = index.entry(k.clone()).or_default();
                for (id, x) in ids.as_object().into_iter().flatten() {
                    let s = |i: usize| x[i].as_str().map(String::from);
                    e.insert(id.clone(), Entry { mine: s(0), peer: s(1), rev: x[2].as_i64().unwrap_or(0), time: x[3].as_i64().unwrap_or(0) });
                }
            }
        }
        let journal = Journal::open(&dir.join("journal.jsonl"));
        let eng = Engine { dir: dir.to_path_buf(), id, name: name.to_string(), kinds, inner: Mutex::new(Inner { journal, index, have, versions: HashMap::new(), dirty: true, peer, stats: Stats::default() }), grew: tokio::sync::Notify::new(), tag: tag.to_string() };
        eng.retry_held();
        eng.save();
        eng
    }
    pub fn id(&self) -> &str { &self.id }
    pub fn name(&self) -> &str { &self.name }
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> { self.inner.lock().unwrap_or_else(|e| e.into_inner()) }
    fn kind(&self, name: &str) -> Option<Arc<dyn Kind>> { self.kinds.iter().find(|k| k.name() == name).cloned() }
    /// Makes compactions happen early (tests).
    pub fn set_journal_limit(&self, bytes: u64) { self.lock().journal.set_limit(bytes); }

    // ---------- this copy's own writes ----------
    /// Looks for this copy's own changes and journals them; how many records it journaled.
    pub fn scan(&self, level: Scan) -> usize {
        let mut inner = self.lock();
        let n: usize = self.kinds.iter().map(|k| self.scan_kind(&mut inner, k, level)).sum();
        drop(inner);
        if n > 0 { self.grew.notify_waiters(); }
        self.save();
        n
    }
    fn scan_kind(&self, inner: &mut Inner, k: &Arc<dyn Kind>, level: Scan) -> usize {
        let name = k.name().to_string();
        let ver = k.version();
        let last = inner.versions.get(&name).copied();
        let moved = ver.is_none() || ver != last;
        let go = match level { Scan::Full => true, Scan::Periodic => moved, Scan::Changed => ver.is_some() && moved };
        if !go { return 0; }
        let (items, whole) = match (level, last, ver) {
            (Scan::Full, _, _) | (_, None, _) | (_, _, None) => (k.snapshot(), true),
            (_, Some(since), Some(_)) => match k.changed(since) { Some(c) => (c, false), None => (k.snapshot(), true) },
        };
        if let Some(v) = ver { inner.versions.insert(name.clone(), v); }
        let t = now(); let mut n = 0;
        let mut seen: HashSet<String> = HashSet::new();
        for (id, v) in items {
            let h = hash(&v);
            if whole { seen.insert(id.clone()); }
            let e = inner.index.entry(name.clone()).or_default().get(&id).cloned().unwrap_or_default();
            if e.mine.as_deref() == Some(h.as_str()) { continue; }
            let (rev, time) = k.stamp(&v).unwrap_or((0, t));
            self.journal(inner, &name, &id, Some(v), rev, time);
            inner.index.entry(name.clone()).or_default().insert(id, Entry { mine: Some(h), rev, time, ..e });
            n += 1;
        }
        if whole && k.deletes() {
            let gone: Vec<(String, Entry)> = inner.index.get(&name).map(|m| m.iter().filter(|(id, e)| e.mine.is_some() && !seen.contains(*id)).map(|(id, e)| (id.clone(), e.clone())).collect()).unwrap_or_default();
            for (id, e) in gone {
                self.journal(inner, &name, &id, None, e.rev, t);
                inner.index.entry(name.clone()).or_default().insert(id, Entry { mine: None, time: t, ..e });
                n += 1;
            }
        }
        if n > 0 { inner.dirty = true; }
        n
    }
    fn journal(&self, inner: &mut Inner, kind: &str, id: &str, value: Option<Value>, rev: i64, time: i64) {
        let r = Record { kind: kind.into(), id: id.into(), rev, time, origin: self.id.clone(), origin_name: self.name.clone(), seq: inner.journal.next_seq(), value };
        if let Err(e) = inner.journal.append(&r) { eprintln!("{} could not write the journal: {e}", self.tag); }
    }

    // ---------- the peer's records ----------
    /// Applies a batch of the peer's records (from the copy `from`, in its journal's order).
    /// `after`: the seq of the sender's journal this batch starts after (what it learnt from hello). When it is below what
    /// this copy thought it had (the sender's journal went back: a restore), this copy takes it from there again.
    pub fn receive(&self, from: &str, after: u64, records: &[Value]) -> Report {
        self.scan(Scan::Periodic); // this copy's own changes first, so a conflict is seen as one
        let mut inner = self.lock();
        let had = inner.have.get(from).copied().unwrap_or(0);
        if after < had { eprintln!("{} the peer's journal starts again after {after} (this copy had up to {had}): taking it from there", self.tag); inner.have.insert(from.to_string(), after); }
        let mut rep = Report { have: inner.have.get(from).copied().unwrap_or(0), ..Default::default() };
        for raw in records {
            let seq = raw["seq"].as_u64().unwrap_or(0);
            let rec = match Record::from_value(raw) {
                Ok(r) => r,
                Err(why) => {
                    // refused, and passed: a bad record never holds up the ones after it
                    if seq > rep.have { inner.have.insert(from.to_string(), seq); rep.have = seq; }
                    self.refuse(&mut inner, &mut rep, seq, raw["kind"].as_str().unwrap_or(""), raw["id"].as_str().unwrap_or(""), &why); continue;
                }
            };
            let have = inner.have.get(&rec.origin).copied().unwrap_or(0);
            if rec.seq <= have { rep.same += 1; continue; } // already had it
            inner.have.insert(rec.origin.clone(), rec.seq);
            if rec.origin == from { rep.have = rec.seq; }
            if rec.origin == self.id { continue; } // never take back what this copy sent (no echo)
            match self.kind(&rec.kind) {
                None => { self.hold(raw); rep.held += 1; inner.stats.held += 1; }
                Some(k) => self.apply(&mut inner, &k, &rec, &mut rep),
            }
            inner.dirty = true;
        }
        if !records.is_empty() { inner.stats.last_in = now(); }
        inner.dirty = true;
        drop(inner);
        self.save();
        rep
    }
    fn refuse(&self, inner: &mut Inner, rep: &mut Report, seq: u64, kind: &str, id: &str, why: &str) {
        eprintln!("{} refused a record (seq {seq}, kind \"{}\", id \"{}\"): {why}", self.tag, kind.chars().take(64).collect::<String>(), id.chars().take(120).collect::<String>());
        inner.stats.refused += 1;
        rep.refused.push((seq, kind.chars().take(64).collect(), id.chars().take(200).collect(), why.to_string()));
    }
    fn apply(&self, inner: &mut Inner, k: &Arc<dyn Kind>, rec: &Record, rep: &mut Report) {
        let name = k.name().to_string();
        if let Some(v) = &rec.value { if let Err(why) = k.check(&rec.id, v) { return self.refuse(inner, rep, rec.seq, &name, &rec.id, &format!("fails the {name} shape check: {why}")); } }
        let mut local = k.get(&rec.id);
        let ih = rec.value.as_ref().map(hash);
        let mut lh = local.as_ref().map(hash);
        let mut e = inner.index.get(&name).and_then(|m| m.get(&rec.id)).cloned().unwrap_or_default();
        let put_entry = |inner: &mut Inner, e: Entry| { inner.index.entry(name.clone()).or_default().insert(rec.id.clone(), e); };
        if lh == ih {
            if e == Entry::default() { e.rev = rec.rev; e.time = rec.time; }
            e.mine = lh; e.peer = ih; rep.same += 1;
            return put_entry(inner, e);
        }
        // two different things under one id (each copy hands out ids): the greater identity moves to a new id, on its own copy
        if let (Some(l), Some(v)) = (&local, &rec.value) {
            if let (Some(a), Some(b)) = (k.identity(l), k.identity(v)) {
                if a != b {
                    if a < b { inner.stats.deferred += 1; rep.deferred.push(rec.seq); return; }
                    match k.rekey(&rec.id) {
                        Some(new_id) => { eprintln!("{} {} {} was another record here: this copy's moved to {new_id}", self.tag, name, rec.id); local = None; lh = None; e = Entry::default(); }
                        None => { inner.stats.deferred += 1; rep.deferred.push(rec.seq); return; }
                    }
                }
            }
        }
        if let Some(v) = &rec.value {
            if let Some(m) = k.merge(&rec.id, local.as_ref(), v) {
                if Some(hash(&m)) != lh {
                    match k.put(&rec.id, &m) { Ok(stored) => { e.mine = Some(hash(&stored)); rep.applied += 1; inner.stats.applied += 1; } Err(why) => return self.refuse(inner, rep, rec.seq, &name, &rec.id, &why) }
                } else { rep.kept.push(rec.seq); }
                e.peer = ih; e.rev = e.rev.max(rec.rev); e.time = e.time.max(rec.time);
                return put_entry(inner, e);
            }
        }
        // newest wins, always (both copies run on one machine, one clock: a later change has the higher order). A true
        // conflict is when this copy changed the record since the peer last had it: then the loser is kept.
        let never = local.is_none() && e == Entry::default();
        let take = never || {
            let lorder = if e.mine == lh && (e.rev, e.time) != (0, 0) { (e.rev, e.time) } else { local.as_ref().and_then(|l| k.stamp(l)).unwrap_or((0, now())) };
            (rec.order(), ih.clone().unwrap_or_default()) > (lorder, lh.clone().unwrap_or_default())
        };
        let plain = never || e.peer == lh;
        if !plain {
            inner.stats.conflicts += 1;
            let peer_name = if rec.origin_name.is_empty() { rec.origin.as_str() } else { rec.origin_name.as_str() };
            let c = if take { Conflict { winner: rec.value.as_ref(), loser: local.as_ref(), winner_from: peer_name, loser_from: &self.name } } else { Conflict { winner: local.as_ref(), loser: rec.value.as_ref(), winner_from: &self.name, loser_from: peer_name } };
            k.keep_loser(&rec.id, &c);
        }
        if take {
            let r = match &rec.value { Some(v) => k.put(&rec.id, v).map(|s| Some(hash(&s))), None => k.remove(&rec.id).map(|_| None) };
            match r { Ok(m) => { e.mine = m; e.rev = rec.rev; e.time = rec.time; rep.applied += 1; inner.stats.applied += 1; } Err(why) => return self.refuse(inner, rep, rec.seq, &name, &rec.id, &why) }
        } else { rep.kept.push(rec.seq); }
        e.peer = ih;
        put_entry(inner, e);
    }
    fn hold(&self, raw: &Value) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("held.jsonl")) { let _ = writeln!(f, "{raw}"); }
    }
    /// Held records of kinds this build knows now are applied (in the order they came); the rest stay held.
    fn retry_held(&self) {
        let f = self.dir.join("held.jsonl");
        let Ok(t) = std::fs::read_to_string(&f) else { return };
        let mut keep = String::new(); let mut done = 0;
        let mut inner = self.lock();
        for line in t.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(raw) = serde_json::from_str::<Value>(line) else { continue };
            match (Record::from_value(&raw), raw["kind"].as_str().and_then(|k| self.kind(k))) {
                (Ok(rec), Some(k)) => { let mut rep = Report::default(); self.apply(&mut inner, &k, &rec, &mut rep); done += 1; }
                _ => { keep.push_str(line); keep.push('\n'); }
            }
        }
        inner.dirty = true;
        drop(inner);
        if done > 0 { eprintln!("{} {done} held record(s) applied: this build knows their kind now", self.tag); }
        if keep.is_empty() { let _ = std::fs::remove_file(&f); } else { let tmp = f.with_extension("jsonl.tmp"); if std::fs::write(&tmp, keep).is_ok() { let _ = std::fs::rename(&tmp, &f); } }
    }
    /// How many records are held (kinds this build does not know).
    pub fn held_count(&self) -> usize { std::fs::read_to_string(self.dir.join("held.jsonl")).map(|t| t.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0) }

    // ---------- the sender's side ----------
    /// This copy's records after `seq`, for the peer (about `max_bytes` at most, at least one).
    pub fn outgoing(&self, after: u64, max_bytes: u64, max_n: usize) -> Vec<Value> { self.lock().journal.after(after, max_bytes, max_n) }
    /// The last seq of this copy's journal.
    pub fn last_seq(&self) -> u64 { self.lock().journal.last_seq() }
    /// The peer took `sent`: for each record it applied (or already had) it is known to have that value now.
    pub fn acked(&self, sent: &[Value], answer: &Value) {
        let skip: HashSet<u64> = ["kept", "deferred"].iter().flat_map(|k| answer[*k].as_array().cloned().unwrap_or_default()).filter_map(|v| v.as_u64())
            .chain(answer["refused"].as_array().into_iter().flatten().filter_map(|r| r["seq"].as_u64())).collect();
        let mut inner = self.lock();
        for v in sent {
            let Ok(r) = Record::from_value(v) else { continue };
            if skip.contains(&r.seq) { continue; }
            let e = inner.index.entry(r.kind.clone()).or_default().entry(r.id.clone()).or_default();
            e.peer = r.value.as_ref().map(hash);
        }
        inner.peer.last_out = now();
        inner.dirty = true;
        drop(inner);
        self.save();
    }
    /// What the sender learnt about the peer (its id, name, how far it has this copy's journal), or that it is down.
    pub fn set_peer(&self, up: bool, hub: &str, name: &str, have: u64, error: &str) {
        let mut inner = self.lock();
        if up {
            if !inner.peer.hub.is_empty() && inner.peer.hub != hub { eprintln!("{} the peer is a different copy now ({} → {hub}): a full exchange follows", self.tag, inner.peer.hub); }
            if inner.peer.hub != hub || inner.peer.name != name { inner.dirty = true; }
            inner.peer.hub = hub.into(); inner.peer.name = name.into(); inner.peer.have = have;
        }
        if inner.peer.up != up { eprintln!("{} the peer is {}{}", self.tag, if up { "up" } else { "down" }, if error.is_empty() { String::new() } else { format!(" ({error})") }); }
        inner.peer.up = up; inner.peer.error = error.into();
    }
    /// How far this copy has the journal of `origin`.
    pub fn have(&self, origin: &str) -> u64 { self.lock().have.get(origin).copied().unwrap_or(0) }
    /// A summary for a status page: the peer, the last exchanges, what waits.
    pub fn status(&self) -> Value {
        let inner = self.lock();
        let p = &inner.peer;
        let queued = inner.journal.count_after(p.have);
        json!({ "hub": self.id, "name": self.name, "peer": { "up": p.up, "hub": p.hub, "name": p.name, "error": if p.error.is_empty() { Value::Null } else { json!(p.error) } },
            "lastSent": if p.last_out > 0 { json!(p.last_out) } else { Value::Null }, "lastReceived": if inner.stats.last_in > 0 { json!(inner.stats.last_in) } else { Value::Null },
            "queued": queued, "journal": inner.journal.last_seq(), "held": self.held_count(), "applied": inner.stats.applied, "refused": inner.stats.refused, "conflicts": inner.stats.conflicts, "deferred": inner.stats.deferred })
    }
    fn save(&self) {
        let mut inner = self.lock();
        if !inner.dirty { return; }
        inner.dirty = false;
        let st = json!({ "hub": self.id, "name": self.name, "have": inner.have, "peer": { "hub": inner.peer.hub, "name": inner.peer.name } });
        let mut idx = Map::new();
        for (k, m) in &inner.index {
            let mut o = Map::new();
            for (id, e) in m { o.insert(id.clone(), json!([e.mine, e.peer, e.rev, e.time])); }
            idx.insert(k.clone(), Value::Object(o));
        }
        drop(inner);
        let write = |f: &str, v: &Value| { let p = self.dir.join(f); let tmp = p.with_extension("json.tmp"); if std::fs::write(&tmp, v.to_string()).is_ok() { pgm_core_internal::fs::private(&tmp); let _ = std::fs::rename(&tmp, &p); } };
        write("state.json", &st);
        write("index.json", &Value::Object(idx));
    }
}
