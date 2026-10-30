//! The write buffer (T-331; team note plans/sessions-always-saved.md § 2): every write of a session or Work file goes
//! through one writer thread instead of each request writing for itself.
//! - **Appends** (journals: one JSON line each) queue per file, in order, and go out in batches: one write and one
//!   fsync per file per batch. **Rewrites** of a whole file coalesce: only the newest content of a file is written
//!   (temp file + rename, as `json::write_json` does), and an older rewrite or append of the same file in the same batch
//!   is not written at all (the rewrite replaces it).
//! - It **watches its load**: writes and bytes per second, queue depth, flush time. A **spike** (this second's writes
//!   over 4x the median of the last 15 minutes, and at least `spike_min` a second, or the queue past `spike_depth`) widens
//!   the batching window (fewer, larger writes), says so once in the log and in `stats()`, and narrows it again when the
//!   spike is over. Past `hard_depth` queued writes a writer waits (back-pressure) instead of memory growing without end.
//! - Nothing queued is dropped: `flush()` waits until everything queued before it is on disk (a graceful shutdown and
//!   any reader of a buffered file call it). A crash loses at most the last batching window, and a journal line is
//!   either whole or absent (`read_lines` skips a torn last line).
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How a buffer batches and when it calls a spike.
#[derive(Clone, Debug)]
pub struct Cfg {
    /// the batching window in quiet times, and during a spike
    pub window: Duration,
    pub spike_window: Duration,
    /// a second with fewer writes than this is never a spike
    pub spike_min: u64,
    /// this many queued writes is a spike whatever the rate
    pub spike_depth: usize,
    /// this many queued writes make a writer wait until half of them are out
    pub hard_depth: usize,
    /// how its log lines start ("[vr-hub] store:")
    pub tag: String,
}
impl Default for Cfg {
    fn default() -> Self { Cfg { window: Duration::from_millis(50), spike_window: Duration::from_millis(250), spike_min: 200, spike_depth: 5_000, hard_depth: 50_000, tag: "store:".into() } }
}

enum Op { Append(PathBuf, Vec<u8>), Replace(PathBuf, Vec<u8>, bool) }
impl Op { fn path(&self) -> &Path { match self { Op::Append(p, _) | Op::Replace(p, _, _) => p } } fn len(&self) -> usize { match self { Op::Append(_, b) | Op::Replace(_, b, _) => b.len() } } }

#[derive(Default)]
struct Stats {
    ops: u64, bytes: u64, batches: u64, files_written: u64, coalesced: u64, errors: u64, throttled: u64,
    max_depth: usize, last_flush_ms: f64, max_flush_ms: f64, spikes: u64, in_spike: bool, spike_since: Option<Instant>,
    /// writes per second, the last 900 seconds: (second, count)
    per_sec: VecDeque<(u64, u64)>,
    last_error: Option<String>,
}
struct Inner { q: VecDeque<(u64, Op)>, next: u64, done: u64, stats: Stats, stopping: bool }

/// A write buffer with its own writer thread. One per process is enough: `global()`.
pub struct WriteBuffer { inner: Mutex<Inner>, wake: Condvar, flushed: Condvar, cfg: Cfg, started: Instant }

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

impl WriteBuffer {
    /// A buffer and its writer thread.
    pub fn new(cfg: Cfg) -> Arc<WriteBuffer> {
        let wb = Arc::new(WriteBuffer { inner: Mutex::new(Inner { q: VecDeque::new(), next: 1, done: 0, stats: Stats::default(), stopping: false }), wake: Condvar::new(), flushed: Condvar::new(), cfg, started: Instant::now() });
        let w = wb.clone();
        let _ = std::thread::Builder::new().name("pgm-store".into()).spawn(move || w.run());
        wb
    }
    fn secs(&self) -> u64 { self.started.elapsed().as_secs() }

    /// Appends bytes to a file (made with its folder when missing).
    pub fn append(&self, path: &Path, bytes: Vec<u8>) { self.push(Op::Append(path.to_path_buf(), bytes)) }
    /// Appends one JSON value as a line (a journal entry).
    pub fn append_line(&self, path: &Path, v: &Value) { let mut b = serde_json::to_vec(v).unwrap_or_default(); b.push(b'\n'); self.append(path, b) }
    /// Replaces a file's whole content (temp file + rename); `private`: readable by its owner only.
    pub fn replace(&self, path: &Path, bytes: Vec<u8>, private: bool) { self.push(Op::Replace(path.to_path_buf(), bytes, private)) }
    /// Replaces a JSON file (pretty, as `json::write_json` writes it).
    pub fn replace_json(&self, path: &Path, v: &Value, private: bool) { self.replace(path, serde_json::to_string_pretty(v).unwrap_or_default().into_bytes(), private) }

    fn push(&self, op: Op) {
        let mut g = lock(&self.inner);
        // back-pressure: a writer waits while the queue is past its hard limit (it never drops anything)
        if g.q.len() >= self.cfg.hard_depth {
            g.stats.throttled += 1;
            while g.q.len() >= self.cfg.hard_depth / 2 && !g.stopping { g = self.flushed.wait_timeout(g, Duration::from_millis(50)).map(|x| x.0).unwrap_or_else(|e| e.into_inner().0); }
        }
        let id = g.next; g.next += 1;
        let (n, sec) = (op.len() as u64, self.secs());
        g.stats.ops += 1; g.stats.bytes += n;
        match g.stats.per_sec.back_mut() { Some(b) if b.0 == sec => b.1 += 1, _ => { g.stats.per_sec.push_back((sec, 1)); while g.stats.per_sec.len() > 900 { g.stats.per_sec.pop_front(); } } }
        g.q.push_back((id, op));
        let d = g.q.len(); if d > g.stats.max_depth { g.stats.max_depth = d; }
        drop(g);
        self.wake.notify_one();
    }

    /// Waits until everything queued before this call is on disk (at most `timeout`); false = it was not in time.
    pub fn flush_for(&self, timeout: Duration) -> bool {
        let t0 = Instant::now();
        let mut g = lock(&self.inner);
        let target = g.next - 1;
        while g.done < target {
            if t0.elapsed() >= timeout { return false; }
            self.wake.notify_one();
            g = self.flushed.wait_timeout(g, Duration::from_millis(20)).map(|x| x.0).unwrap_or_else(|e| e.into_inner().0);
        }
        true
    }
    /// `flush_for` 10 seconds.
    pub fn flush(&self) -> bool { self.flush_for(Duration::from_secs(10)) }
    /// Writes what is queued and stops taking more (the process is going).
    pub fn shutdown(&self) -> bool { let ok = self.flush(); lock(&self.inner).stopping = true; self.wake.notify_all(); ok }

    /// Is this second (or the queue) a spike? (the rate against the median of the last 15 minutes)
    fn spiking(&self, s: &Stats, depth: usize) -> bool {
        if depth >= self.cfg.spike_depth { return true; }
        let now = self.secs();
        let cur = s.per_sec.iter().rev().find(|b| b.0 + 1 >= now).map(|b| b.1).unwrap_or(0);
        if cur < self.cfg.spike_min { return false; }
        let mut past: Vec<u64> = s.per_sec.iter().filter(|b| b.0 + 1 < now).map(|b| b.1).collect();
        // the seconds with no writes count too (a quiet hub's median is 0 or 1)
        let span = (s.per_sec.front().map(|b| now.saturating_sub(b.0)).unwrap_or(0) as usize).clamp(1, 900);
        past.resize(span.max(past.len()), 0);
        past.sort_unstable();
        let median = past[past.len() / 2].max(1);
        cur > median * 4
    }

    fn run(self: Arc<Self>) {
        loop {
            // wait for work
            let window = {
                let mut g = lock(&self.inner);
                while g.q.is_empty() && !g.stopping { g = self.wake.wait_timeout(g, Duration::from_millis(500)).map(|x| x.0).unwrap_or_else(|e| e.into_inner().0); }
                if g.q.is_empty() && g.stopping { return; }
                let depth = g.q.len();
                let spike = self.spiking(&g.stats, depth);
                if spike != g.stats.in_spike {
                    g.stats.in_spike = spike;
                    if spike { g.stats.spikes += 1; g.stats.spike_since = Some(Instant::now()); eprintln!("{} write spike: {} queued, batching every {} ms until it settles", self.cfg.tag, depth, self.cfg.spike_window.as_millis()); }
                    else { let took = g.stats.spike_since.take().map(|t| t.elapsed().as_secs()).unwrap_or(0); eprintln!("{} write spike over after {took} s; batching every {} ms again", self.cfg.tag, self.cfg.window.as_millis()); }
                }
                if spike { self.cfg.spike_window } else { self.cfg.window }
            };
            // let a batch gather (a flush wakes it early: it does not wait out the window for a reader)
            std::thread::sleep(window);
            let (ops, last_id): (Vec<Op>, u64) = { let mut g = lock(&self.inner); let last = g.q.back().map(|x| x.0).unwrap_or(g.done); (g.q.drain(..).map(|x| x.1).collect(), last) };
            let t0 = Instant::now();
            let (files, coalesced, errs) = write_batch(ops);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let mut g = lock(&self.inner);
            g.done = last_id; g.stats.batches += 1; g.stats.files_written += files as u64; g.stats.coalesced += coalesced as u64;
            g.stats.last_flush_ms = ms; if ms > g.stats.max_flush_ms { g.stats.max_flush_ms = ms; }
            for e in errs { g.stats.errors += 1; eprintln!("{} {e}", self.cfg.tag); g.stats.last_error = Some(e); }
            drop(g);
            self.flushed.notify_all();
        }
    }

    /// What it has done and how it is now (for the metrics: § 4).
    pub fn stats(&self) -> Value {
        let g = lock(&self.inner); let s = &g.stats; let now = self.secs();
        let last_min: u64 = s.per_sec.iter().filter(|b| b.0 + 60 >= now).map(|b| b.1).sum();
        json!({ "queued": g.q.len(), "maxQueued": s.max_depth, "writes": s.ops, "bytes": s.bytes, "batches": s.batches, "filesWritten": s.files_written,
                "coalesced": s.coalesced, "errors": s.errors, "lastError": s.last_error, "throttled": s.throttled, "lastFlushMs": (s.last_flush_ms * 10.0).round() / 10.0,
                "maxFlushMs": (s.max_flush_ms * 10.0).round() / 10.0, "writesLastMinute": last_min, "spikes": s.spikes, "inSpike": s.in_spike })
    }
}

/// One batch: per file, only what is still needed (the last rewrite, then the appends after it), in order of first use.
fn write_batch(ops: Vec<Op>) -> (usize, usize, Vec<String>) {
    let mut order: Vec<PathBuf> = vec![];
    let mut per: HashMap<PathBuf, (Option<(Vec<u8>, bool)>, Vec<u8>, bool)> = HashMap::new(); // rewrite, appends after it, appends only
    let mut coalesced = 0;
    for op in ops {
        let p = op.path().to_path_buf();
        let e = per.entry(p.clone()).or_insert_with(|| { order.push(p.clone()); (None, vec![], false) });
        match op {
            Op::Replace(_, b, private) => { if e.0.is_some() || !e.1.is_empty() { coalesced += 1; } e.0 = Some((b, private)); e.1.clear(); }
            Op::Append(_, b) => { if !e.1.is_empty() { coalesced += 1; } e.1.extend_from_slice(&b); e.2 = e.0.is_none(); }
        }
    }
    let mut errs = vec![];
    for p in &order {
        let (rewrite, tail, _) = per.remove(p).unwrap_or((None, vec![], false));
        if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        if let Some((mut body, private)) = rewrite {
            body.extend_from_slice(&tail);
            let tmp = p.with_extension(format!("{}.tmp", p.extension().and_then(|x| x.to_str()).unwrap_or("w")));
            let r = (|| -> std::io::Result<()> { let mut f = std::fs::File::create(&tmp)?; f.write_all(&body)?; f.sync_data()?; drop(f); if private { crate::fs::private(&tmp); } std::fs::rename(&tmp, p) })();
            if let Err(e) = r { errs.push(format!("could not write {}: {e}", p.display())); }
        } else if !tail.is_empty() {
            let r = (|| -> std::io::Result<()> { let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?; f.write_all(&tail)?; f.sync_data() })();
            if let Err(e) = r { errs.push(format!("could not append to {}: {e}", p.display())); }
        }
    }
    (order.len(), coalesced, errs)
}

/// The JSON lines of a journal, oldest first; a torn last line (a crash in the middle of a write) is left out.
pub fn read_lines(path: &Path) -> Vec<Value> {
    let Ok(t) = std::fs::read_to_string(path) else { return vec![] };
    t.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect()
}

/// The hot cache of journals (§ 1): a journal read once stays in memory while it is used; as journals only grow, a read
/// after an append takes in only the new tail. One unused for `HOT_FOR` goes, and past `HOT_BYTES` in all the least
/// recently used go first. The disk always has everything: this only saves reading it again.
pub const HOT_FOR: Duration = Duration::from_secs(8 * 3600);
pub const HOT_BYTES: u64 = 64 * 1024 * 1024;
struct Hot { len: u64, lines: Vec<Value>, used: Instant }
fn hot() -> &'static Mutex<HashMap<PathBuf, Hot>> { static H: OnceLock<Mutex<HashMap<PathBuf, Hot>>> = OnceLock::new(); H.get_or_init(Mutex::default) }
/// `read_lines` through the hot cache (the global buffer is flushed first, so what was queued is in it).
pub fn read_lines_hot(path: &Path) -> Vec<Value> {
    global().flush();
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut h = lock(hot());
    let now = Instant::now();
    h.retain(|_, x| now.duration_since(x.used) < HOT_FOR);
    if len == 0 { h.remove(path); return vec![]; }
    let fresh = match h.get_mut(path) {
        Some(x) if x.len == len => { x.used = now; return x.lines.clone(); }
        Some(x) if x.len < len => {
            // only the new tail (whole lines only: a line still being written is read next time)
            let tail = (|| -> std::io::Result<Vec<u8>> { use std::io::{Read, Seek}; let mut f = std::fs::File::open(path)?; f.seek(std::io::SeekFrom::Start(x.len))?; let mut b = vec![]; f.read_to_end(&mut b)?; Ok(b) })().unwrap_or_default();
            let whole = tail.iter().rposition(|c| *c == b'\n').map(|i| i + 1).unwrap_or(0);
            x.lines.extend(String::from_utf8_lossy(&tail[..whole]).lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()));
            x.len += whole as u64; x.used = now;
            return x.lines.clone();
        }
        _ => true, // new, or the file was replaced (shorter): read it whole
    };
    if fresh {
        let t = std::fs::read(path).unwrap_or_default();
        let whole = t.iter().rposition(|c| *c == b'\n').map(|i| i + 1).unwrap_or(0);
        let lines: Vec<Value> = String::from_utf8_lossy(&t[..whole]).lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect();
        h.insert(path.to_path_buf(), Hot { len: whole as u64, lines: lines.clone(), used: now });
        // past the size cap: the least recently used go
        let mut total: u64 = h.values().map(|x| x.len).sum();
        while total > HOT_BYTES && h.len() > 1 {
            let Some(old) = h.iter().filter(|(p, _)| p.as_path() != path).min_by_key(|(_, x)| x.used).map(|(p, _)| p.clone()) else { break };
            if let Some(x) = h.remove(&old) { total -= x.len; }
        }
        return lines;
    }
    vec![]
}
/// A journal is gone (its session was deleted): out of the cache too.
pub fn forget_hot(path: &Path) { lock(hot()).remove(path); }

/// The process's own buffer (one writer thread for the whole hub).
pub fn global() -> &'static Arc<WriteBuffer> {
    static G: OnceLock<Arc<WriteBuffer>> = OnceLock::new();
    G.get_or_init(|| WriteBuffer::new(Cfg { tag: "[store]".into(), ..Cfg::default() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dir(n: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("pgm-store-{n}-{}", crate::crypto::random_hex(4))); std::fs::create_dir_all(&d).unwrap(); d }
    fn quick() -> Cfg { Cfg { window: Duration::from_millis(5), spike_window: Duration::from_millis(30), spike_min: 50, spike_depth: 100_000, hard_depth: 100_000, tag: "[test]".into() } }

    #[test]
    fn many_writers_land_in_order_and_complete() {
        let d = dir("order"); let wb = WriteBuffer::new(quick());
        let hs: Vec<_> = (0..50).map(|w| { let (wb, d) = (wb.clone(), d.clone()); std::thread::spawn(move || for i in 0..200 { wb.append_line(&d.join(format!("w{}.jsonl", w % 5)), &json!({ "w": w, "i": i })); }) }).collect();
        for h in hs { h.join().unwrap(); }
        assert!(wb.flush());
        let mut total = 0;
        for f in 0..5 {
            let lines = read_lines(&d.join(format!("w{f}.jsonl"))); total += lines.len();
            // each writer's own lines are in the order it wrote them
            let mut last: HashMap<i64, i64> = HashMap::new();
            for l in &lines { let (w, i) = (l["w"].as_i64().unwrap(), l["i"].as_i64().unwrap()); assert!(last.get(&w).is_none_or(|p| *p < i)); last.insert(w, i); }
        }
        assert_eq!(total, 10_000);
        assert_eq!(wb.stats()["writes"], json!(10_000));
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_rewrite_coalesces_and_only_the_newest_content_is_written() {
        let d = dir("coal"); let wb = WriteBuffer::new(Cfg { window: Duration::from_millis(80), ..quick() });
        let f = d.join("state.json");
        for i in 0..20 { wb.replace_json(&f, &json!({ "n": i }), false); }
        assert!(wb.flush());
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&f).unwrap()).unwrap()["n"], json!(19));
        assert!(wb.stats()["coalesced"].as_u64().unwrap() >= 10, "{}", wb.stats());
        assert!(!d.join("state.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_burst_is_called_a_spike_and_absorbed() {
        let d = dir("spike"); let wb = WriteBuffer::new(Cfg { spike_min: 300, ..quick() });
        let f = d.join("j.jsonl");
        for i in 0..20 { wb.append_line(&f, &json!({ "quiet": i })); std::thread::sleep(Duration::from_millis(20)); }
        for i in 0..5_000 { wb.append_line(&f, &json!({ "burst": i })); }
        assert!(wb.flush());
        let st = wb.stats();
        assert!(st["spikes"].as_u64().unwrap() >= 1, "{st}");
        assert_eq!(read_lines(&f).len(), 5_020);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn past_the_hard_limit_a_writer_waits_and_nothing_is_dropped() {
        let d = dir("hard"); let wb = WriteBuffer::new(Cfg { hard_depth: 200, window: Duration::from_millis(20), ..quick() });
        let f = d.join("j.jsonl");
        for i in 0..3_000 { wb.append_line(&f, &json!({ "i": i })); }
        assert!(wb.flush());
        assert_eq!(read_lines(&f).len(), 3_000);
        assert!(wb.stats()["throttled"].as_u64().unwrap() >= 1);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn the_hot_cache_takes_in_only_new_whole_lines_and_sees_a_replaced_file() {
        let d = dir("hot"); let f = d.join("j.jsonl");
        global().append_line(&f, &json!({ "a": 1 })); global().append_line(&f, &json!({ "a": 2 }));
        assert_eq!(read_lines_hot(&f).len(), 2);
        global().append_line(&f, &json!({ "a": 3 }));
        assert_eq!(read_lines_hot(&f), vec![json!({ "a": 1 }), json!({ "a": 2 }), json!({ "a": 3 })]);
        // a line being written (no newline yet) is not read; once whole, it is
        std::fs::OpenOptions::new().append(true).open(&f).unwrap().write_all(b"{\"a\":").unwrap();
        assert_eq!(read_lines_hot(&f).len(), 3);
        std::fs::OpenOptions::new().append(true).open(&f).unwrap().write_all(b"4}\n").unwrap();
        assert_eq!(read_lines_hot(&f).last(), Some(&json!({ "a": 4 })));
        std::fs::write(&f, "{\"b\":1}\n").unwrap(); // replaced (shorter): read whole again
        assert_eq!(read_lines_hot(&f), vec![json!({ "b": 1 })]);
        let _ = std::fs::remove_file(&f); assert!(read_lines_hot(&f).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_torn_last_line_is_left_out() {
        let d = dir("torn"); let f = d.join("j.jsonl");
        std::fs::write(&f, "{\"a\":1}\n{\"a\":2}\n{\"a\":").unwrap();
        assert_eq!(read_lines(&f), vec![json!({ "a": 1 }), json!({ "a": 2 })]);
        assert!(read_lines(&d.join("missing.jsonl")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
