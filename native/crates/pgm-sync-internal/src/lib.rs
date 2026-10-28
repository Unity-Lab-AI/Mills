//! **pgm-sync-internal** = PGM_Sync_Internal (PsyGraphMediaSyncInternal): two full copies of a program's data (two
//! hubs on one machine: live and dev) kept the same, both ways, continuously, never losing data.
//! - [`record`]: a change record `{ kind, id, rev, time, origin, originName, seq, value | deleted }` and its shape check
//! - [`journal`]: each copy's own writes, one record per line, add-only (compacted to the newest per record when big)
//! - [`engine`]: the [`Kind`] adapters a program plugs its data in with; the scan that notices the copy's own writes;
//!   applying the peer's records: newest wins per record (rev, then time, then the value's hash), a true conflict keeps
//!   the loser (the kind decides where: history, activity, a copy), a kind's own merge, two records under one id moved
//!   apart, tombstones, refused malformed records, records of unknown kinds held until a build knows them; no echo (a
//!   copy never takes back what it sent); a cursor per origin, so a copy that was down catches up
//! - [`wire`]: the exchange over HTTP on this machine with a shared secret (hello + push, both ways), the gate (this
//!   machine only, never through a proxy, constant-time secret), the scan and send loops
//! - [`canon`]: canonical JSON and the hash of a value
//!
//! Nothing here knows a particular program: the hub's board, notes, chat and accounts are adapters in the hub.
pub mod canon;
pub mod engine;
pub mod journal;
pub mod record;
pub mod wire;

pub use engine::{Conflict, Engine, Kind, Report, Scan};
pub use record::Record;

/// The secret two copies share: the file's first line, trimmed; None when it is missing or shorter than 16 characters.
pub fn read_secret(file: &std::path::Path) -> Option<String> {
    let t = std::fs::read_to_string(file).ok()?;
    let s = t.lines().next().unwrap_or("").trim().to_string();
    (s.len() >= 16).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    /// A kind in memory: values carry `rev` / `t` (their order) and maybe `born` (their identity); a lost value goes to `lost`.
    #[derive(Default)]
    struct Mem { name: String, data: Mutex<BTreeMap<String, Value>>, ver: Mutex<u64>, lost: Mutex<Vec<(String, Value, String)>>, deletes: bool, merge_max: bool }
    impl Mem {
        fn new(name: &str) -> Arc<Mem> { Arc::new(Mem { name: name.into(), ..Default::default() }) }
        fn set(&self, id: &str, v: Value) { self.data.lock().unwrap().insert(id.into(), v); *self.ver.lock().unwrap() += 1; }
        fn del(&self, id: &str) { self.data.lock().unwrap().remove(id); *self.ver.lock().unwrap() += 1; }
        fn val(&self, id: &str) -> Option<Value> { self.data.lock().unwrap().get(id).cloned() }
    }
    impl Kind for Mem {
        fn name(&self) -> &str { &self.name }
        fn version(&self) -> Option<u64> { Some(*self.ver.lock().unwrap()) }
        fn snapshot(&self) -> Vec<(String, Value)> { self.data.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect() }
        fn deletes(&self) -> bool { self.deletes }
        fn get(&self, id: &str) -> Option<Value> { self.val(id) }
        fn check(&self, _id: &str, v: &Value) -> Result<(), String> { if v.is_object() && v.get("bad").is_none() { Ok(()) } else { Err("not a good object".into()) } }
        fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { if self.merge_max { None } else { Some((v["rev"].as_i64().unwrap_or(0), v["t"].as_i64().unwrap_or(0))) } }
        fn identity(&self, v: &Value) -> Option<String> { v["born"].as_str().map(String::from) }
        fn rekey(&self, id: &str) -> Option<String> { let mut d = self.data.lock().unwrap(); let v = d.remove(id)?; let n = format!("{id}-moved"); d.insert(n.clone(), v); *self.ver.lock().unwrap() += 1; Some(n) }
        fn merge(&self, _id: &str, local: Option<&Value>, inc: &Value) -> Option<Value> { if !self.merge_max { return None; } let a = local.and_then(|l| l["n"].as_i64()).unwrap_or(0); Some(json!({ "n": a.max(inc["n"].as_i64().unwrap_or(0)) })) }
        fn put(&self, id: &str, v: &Value) -> Result<Value, String> { self.set(id, v.clone()); Ok(v.clone()) }
        fn remove(&self, id: &str) -> Result<(), String> { self.del(id); Ok(()) }
        fn keep_loser(&self, id: &str, c: &Conflict) { if let Some(l) = c.loser { self.lost.lock().unwrap().push((id.into(), l.clone(), c.loser_from.into())); } }
    }
    struct Side { e: Engine, k: Arc<Mem>, dir: std::path::PathBuf }
    fn side(tag: &str, name: &str, mk: impl Fn() -> Arc<Mem>) -> Side {
        let dir = std::env::temp_dir().join(format!("pgm-sync-{tag}-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let k = mk();
        Side { e: Engine::open(&dir, name, vec![k.clone() as Arc<dyn Kind>], "[test]"), k, dir }
    }
    fn reopen(s: Side, kinds: Vec<Arc<dyn Kind>>) -> Engine { Engine::open(&s.dir, s.e.name(), kinds, "[test]") }
    /// What the send loop does once: a's records after what b has, into b; a hears the answer.
    fn push(a: &Engine, b: &Engine) -> Report {
        a.scan(Scan::Periodic);
        let after = b.have(a.id());
        let recs = a.outgoing(after, 1 << 30, 100_000);
        let rep = b.receive(a.id(), after, &recs);
        a.acked(&recs, &rep.to_value());
        rep
    }
    fn both(a: &Side, b: &Side) { for _ in 0..3 { push(&a.e, &b.e); push(&b.e, &a.e); } }
    fn done(s: &[&Side]) { for x in s { let _ = std::fs::remove_dir_all(&x.dir); } }

    #[test]
    fn a_change_on_either_side_reaches_the_other_and_is_never_sent_back() {
        let (a, b) = (side("basic", "live", || Mem::new("task")), side("basic", "dev", || Mem::new("task")));
        a.k.set("T-1", json!({ "rev": 1, "t": 10, "title": "from live" }));
        b.k.set("T-2", json!({ "rev": 1, "t": 11, "title": "from dev" }));
        both(&a, &b);
        assert_eq!(b.k.val("T-1").unwrap()["title"], "from live");
        assert_eq!(a.k.val("T-2").unwrap()["title"], "from dev");
        // no echo: after the exchange neither journal holds the other's record, and another round sends nothing new
        let (la, lb) = (a.e.last_seq(), b.e.last_seq());
        assert_eq!((la, lb), (1, 1), "each journal has its own write only");
        both(&a, &b);
        assert_eq!((a.e.last_seq(), b.e.last_seq()), (la, lb), "applying the peer's record journals nothing (it would echo back)");
        // a record of this copy's own that comes back is ignored
        let mine = a.e.outgoing(0, 1 << 20, 10);
        let rep = a.e.receive(b.e.id(), b.e.have(a.e.id()), &mine);
        assert_eq!((rep.applied, a.e.last_seq()), (0, la));
        done(&[&a, &b]);
    }

    #[test]
    fn simultaneous_edits_end_equal_on_both_sides_and_the_loser_is_kept() {
        let (a, b) = (side("conflict", "live", || Mem::new("task")), side("conflict", "dev", || Mem::new("task")));
        a.k.set("T-1", json!({ "rev": 1, "t": 10, "title": "start" }));
        both(&a, &b);
        // both change T-1 before they hear of each other: dev's is newer (same rev, later time)
        a.k.set("T-1", json!({ "rev": 2, "t": 20, "title": "live's edit" }));
        b.k.set("T-1", json!({ "rev": 2, "t": 25, "title": "dev's edit" }));
        a.e.scan(Scan::Periodic); b.e.scan(Scan::Periodic);
        both(&a, &b);
        assert_eq!(a.k.val("T-1"), b.k.val("T-1"), "equal on both");
        assert_eq!(a.k.val("T-1").unwrap()["title"], "dev's edit", "newest wins");
        let la = a.k.lost.lock().unwrap().clone(); let lb = b.k.lost.lock().unwrap().clone();
        assert!(la.iter().any(|(id, v, from)| id == "T-1" && v["title"] == "live's edit" && from == "live"), "live keeps its losing edit: {la:?}");
        assert!(lb.iter().any(|(_, v, _)| v["title"] == "live's edit"), "dev keeps the same loser: {lb:?}");
        // a plain later edit is no conflict
        let before = la.len();
        a.k.set("T-1", json!({ "rev": 3, "t": 30, "title": "after" }));
        both(&a, &b);
        assert_eq!(b.k.val("T-1").unwrap()["title"], "after");
        assert_eq!(a.k.lost.lock().unwrap().len() + b.k.lost.lock().unwrap().len(), before + lb.len(), "nothing more lost");
        // a higher rev beats a later time
        a.k.set("T-1", json!({ "rev": 5, "t": 40, "title": "rev 5" }));
        b.k.set("T-1", json!({ "rev": 4, "t": 99, "title": "rev 4 but later" }));
        both(&a, &b);
        assert_eq!((a.k.val("T-1").unwrap()["title"].clone(), b.k.val("T-1").unwrap()["title"].clone()), (json!("rev 5"), json!("rev 5")));
        done(&[&a, &b]);
    }

    #[test]
    fn a_malformed_record_is_refused_and_never_applied_and_does_not_hold_up_the_rest() {
        let (a, b) = (side("bad", "live", || Mem::new("task")), side("bad", "dev", || Mem::new("task")));
        let good = json!({ "kind": "task", "id": "T-9", "rev": 1, "time": 1, "origin": "hX", "originName": "x", "seq": 3, "value": { "title": "fine" } });
        let recs = vec![
            json!({ "kind": "task", "id": "T-1", "origin": "hX", "seq": 1 }),                                             // no value
            json!({ "kind": "task", "id": "T-2", "origin": "hX", "seq": 2, "value": { "bad": true } }),                   // fails the kind's check
            good,
        ];
        let rep = b.e.receive("hX", 0, &recs);
        assert_eq!(rep.refused.len(), 2, "{rep:?}");
        assert!(b.k.val("T-1").is_none() && b.k.val("T-2").is_none(), "never applied");
        assert_eq!(b.k.val("T-9").unwrap()["title"], "fine", "the good one after them is");
        assert_eq!((rep.have, b.e.have("hX")), (3, 3), "the cursor passes them");
        let _ = a;
        done(&[&a, &b]);
    }

    #[test]
    fn a_kind_this_build_does_not_know_is_held_and_applied_once_a_build_knows_it() {
        let (a, b) = (side("unknown", "dev", || Mem::new("task")), side("unknown", "live", || Mem::new("task")));
        let rec = json!({ "kind": "work.session", "id": "s1", "rev": 1, "time": 5, "origin": a.e.id(), "originName": "dev", "seq": 1, "value": { "title": "made on dev" } });
        let rep = b.e.receive(a.e.id(), 0, &[rec]);
        assert_eq!((rep.held, rep.refused.len(), b.e.held_count()), (1, 0, 1), "kept, not dropped, not refused");
        assert_eq!(b.e.have(a.e.id()), 1);
        // the newer build knows the kind: the held record is applied when it opens
        let sessions = Mem::new("work.session");
        let tasks = b.k.clone();
        let b2 = reopen(b, vec![tasks as Arc<dyn Kind>, sessions.clone() as Arc<dyn Kind>]);
        assert_eq!(sessions.val("s1").unwrap()["title"], "made on dev");
        assert_eq!(b2.held_count(), 0);
        let _ = std::fs::remove_dir_all(&a.dir);
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("pgm-sync-unknown-live-{}", std::process::id())));
    }

    #[test]
    fn a_copy_that_was_away_catches_up_and_a_restart_keeps_the_cursor() {
        let (a, b) = (side("catchup", "live", || Mem::new("task")), side("catchup", "dev", || Mem::new("task")));
        a.k.set("T-1", json!({ "rev": 1, "t": 1 }));
        both(&a, &b);
        // dev is down: live changes things, they wait in its journal
        for i in 2..=5 { a.k.set(&format!("T-{i}"), json!({ "rev": 1, "t": i })); a.e.scan(Scan::Changed); }
        a.k.set("T-1", json!({ "rev": 2, "t": 9 })); a.e.scan(Scan::Changed);
        // dev restarts (its engine reopens from its files) and catches up from its own cursor
        let k = b.k.clone();
        let b2 = Side { e: reopen(b, vec![k.clone() as Arc<dyn Kind>]), k, dir: std::env::temp_dir().join(format!("pgm-sync-catchup-dev-{}", std::process::id())) };
        assert_eq!(b2.e.have(a.e.id()), 1, "the cursor survived the restart");
        let rep = push(&a.e, &b2.e);
        assert_eq!(rep.applied, 5);
        assert!((2..=5).all(|i| b2.k.val(&format!("T-{i}")).is_some()) && b2.k.val("T-1").unwrap()["rev"] == 2);
        done(&[&a, &b2]);
    }

    #[test]
    fn deletes_travel_as_tombstones_and_two_records_under_one_id_are_moved_apart() {
        let mk = || Arc::new(Mem { name: "note".into(), deletes: true, ..Default::default() });
        let (a, b) = (side("tomb", "live", mk), side("tomb", "dev", mk));
        a.k.set("x.md", json!({ "rev": 0, "t": 5, "text": "hello" }));
        both(&a, &b);
        a.k.del("x.md");
        a.e.scan(Scan::Full);
        both(&a, &b);
        assert!(b.k.val("x.md").is_none(), "the delete reached dev");
        // both copies hand out ids: each made a "T-7" of its own; the greater identity moves, on its own copy, and both end with both
        let (c, d) = (side("ids", "live", || Mem::new("task")), side("ids", "dev", || Mem::new("task")));
        c.k.set("T-7", json!({ "rev": 1, "t": 1, "born": "1|ann", "title": "live's" }));
        d.k.set("T-7", json!({ "rev": 1, "t": 2, "born": "2|bob", "title": "dev's" }));
        both(&c, &d);
        for s in [&c, &d] {
            assert_eq!(s.k.val("T-7").unwrap()["title"], "live's", "{}", s.e.name());
            assert_eq!(s.k.val("T-7-moved").unwrap()["title"], "dev's", "{}", s.e.name());
        }
        done(&[&a, &b, &c, &d]);
    }

    #[test]
    fn a_kind_with_its_own_merge_takes_the_merged_value_on_both_sides() {
        let mk = || Arc::new(Mem { name: "seen".into(), merge_max: true, ..Default::default() });
        let (a, b) = (side("merge", "live", mk), side("merge", "dev", mk));
        a.k.set("ann|room", json!({ "n": 5 }));
        b.k.set("ann|room", json!({ "n": 3 }));
        both(&a, &b);
        assert_eq!((a.k.val("ann|room").unwrap(), b.k.val("ann|room").unwrap()), (json!({ "n": 5 }), json!({ "n": 5 })));
        done(&[&a, &b]);
    }

    #[test]
    fn the_gate_wants_this_machine_no_proxy_and_the_secret() {
        let secret = "s3cret-s3cret-s3cret";
        let lo: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let far: std::net::SocketAddr = "203.0.113.5:5000".parse().unwrap();
        let with = |pairs: Vec<(&'static str, &'static str)>| move |k: &str| pairs.iter().find(|(a, _)| *a == k).map(|(_, v)| v.to_string());
        assert!(wire::gate(lo, &with(vec![(wire::SECRET_HEADER, secret)]), secret).is_ok());
        assert_eq!(wire::gate(lo, &with(vec![]), secret).unwrap_err().0, 403, "no secret");
        assert!(wire::gate(lo, &with(vec![(wire::SECRET_HEADER, "s3cret-s3cret-s3creX")]), secret).is_err(), "wrong secret");
        assert!(wire::gate(far, &with(vec![(wire::SECRET_HEADER, secret)]), secret).is_err(), "another machine");
        assert!(wire::gate(lo, &with(vec![(wire::SECRET_HEADER, secret), ("x-forwarded-for", "1.2.3.4")]), secret).is_err(), "through nginx");
        assert!(wire::gate(lo, &with(vec![(wire::SECRET_HEADER, "")]), "").is_err(), "no secret configured");
        // a push naming this hub itself, and junk, are refused
        let s = side("gate", "live", || Mem::new("task"));
        assert_eq!(wire::answer(&s.e, "POST", "push", "", json!({ "hub": s.e.id(), "records": [] }).to_string().as_bytes()).0, 400);
        assert_eq!(wire::answer(&s.e, "POST", "push", "", b"nope").0, 400);
        assert_eq!(wire::answer(&s.e, "GET", "hello", "hZ", b"").1["have"], 0);
        done(&[&s]);
    }
}
