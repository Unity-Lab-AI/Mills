# pgm-sync-internal (PGM_Sync_Internal)

Two full copies of a program's data (the live hub and the dev hub on one machine) kept the same, both ways,
continuously, never losing data (board T-324; the team note plans/dev-hub.md, sections 2 and 2b).

```rust
struct Tasks(/* the program's store */);
impl Kind for Tasks {
    fn name(&self) -> &str { "board.task" }
    fn version(&self) -> Option<u64> { Some(/* moves on every write */ 0) }
    fn snapshot(&self) -> Vec<(String, Value)> { /* every record: (id, value) */ vec![] }
    fn get(&self, id: &str) -> Option<Value> { None }
    fn check(&self, id: &str, v: &Value) -> Result<(), String> { Ok(()) }       // the shape check
    fn stamp(&self, _id: &str, v: &Value) -> Option<(i64, i64)> { Some((v["rev"].as_i64()?, v["updatedAt"].as_i64()?)) }
    fn put(&self, id: &str, v: &Value) -> Result<Value, String> { Ok(v.clone()) } // store it (and wake the readers)
    fn keep_loser(&self, id: &str, c: &Conflict) { /* the losing value goes into the record's history */ }
}
let engine = Arc::new(Engine::open(&dir, "live", vec![Arc::new(Tasks(..))], "[hub] sync:"));
tokio::spawn(wire::scan_loop(engine.clone()));                       // notices this copy's own writes
tokio::spawn(wire::send_loop(engine.clone(), peer_url, secret));     // pushes them to the peer
// and the routes: /__sync/<rest> → wire::gate(peer, headers, secret)?, then wire::answer(&engine, method, rest, hub, body)
```

## Rules

- **Change records** `{ kind, id, rev, time, origin, originName, seq, value }` (or `deleted: true`). Each copy journals
  only its own writes (`journal.jsonl`, add-only; compacted to the newest record per id past 256 MB) and pushes the
  records after the peer's cursor; the peer keeps a cursor per origin (`state.json`), so a copy that was down catches up,
  and a peer whose journal went back (a restore) is taken from where it is.
- **No echo**: applying the peer's record journals nothing (the index remembers the value as this copy's own), and a
  record whose origin is this copy is ignored.
- **Newest wins** per record by (rev, time), then the value's hash (both sides decide the same). A **true conflict**
  (this copy changed the record since the peer last had it) calls `keep_loser`: the losing value is kept by the kind.
- A kind may **merge** instead (`merge`), say how two records under one id are the **same thing** (`identity`: two
  copies that both hand out ids; the greater identity moves to a new id with `rekey`, on its own copy), and **delete**
  (`deletes` + `remove`: tombstones).
- **Shape checks**: the generic one (`Record::from_value`) and the kind's (`check`). A malformed record is refused and
  logged, never applied, and does not hold up the records after it.
- **Unknown kinds** are held (`held.jsonl`) and applied when a build that knows the kind opens the engine.
- **The wire** (`wire`): `GET /__sync/hello`, `POST /__sync/push`, on 127.0.0.1 only, never through a proxy (a forwarding
  header is refused), with the shared secret in `x-vr-sync-secret` (constant-time compare).
