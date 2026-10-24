//! Replay: an exact-match response cache keyed by (provider, model, canonical request). It makes a retry after a
//! transport failure free, makes tests deterministic, and answers an identical re-ask without paying again. On top of
//! it sits an opt-in semantic near-miss lookup that is allowed ONLY for plain questions (never for edits, tool use or
//! anything that mentions a file), because a near-miss on an edit request would silently do the wrong thing.
//!
//! Nothing here touches the network. Stored files are plain JSON, one per key, written atomically (temp file and
//! rename) with mode 0600 on unix. A caller that serves a semantic hit MUST tell the user the answer was replayed.
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Keys removed (at any depth) before hashing, because they vary between two identical asks without changing the
/// answer: `stream`, `metadata`, `user`, `session_id`, `request_id`, `idempotency_key`, every key starting with `x-`
/// (case-insensitive), and `cache_control` (a caching hint, not part of the question).
const VOLATILE: &[&str] = &[
    "stream",
    "metadata",
    "user",
    "session_id",
    "request_id",
    "idempotency_key",
    "cache_control",
];

fn volatile(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    VOLATILE.contains(&k.as_str()) || k.starts_with("x-")
}

fn canon(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().filter(|k| !volatile(k)).collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                canon(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canon(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// SHA-256 hex over provider, model and the canonical request (keys sorted recursively, volatile keys removed, see
/// the list above). Array order is kept: message order is meaning.
pub fn canonical_key(provider: &str, model: &str, request: &Value) -> String {
    let mut body = String::new();
    canon(request, &mut body);
    let mut h = Sha256::new();
    h.update(provider.as_bytes());
    h.update([0]);
    h.update(model.as_bytes());
    h.update([0]);
    h.update(body.as_bytes());
    hex::encode(h.finalize())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    Exact,
    Semantic,
}

/// One stored answer: the stream events (or the single response) exactly as received, so replaying is byte-faithful.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    pub provider: String,
    pub model: String,
    pub created_ms: u64,
    pub events: Vec<Value>,
    pub usage: Option<Value>,
    pub prompt_hash: String,
    pub kind: EntryKind,
    /// The question's embedding, kept only when the semantic lookup is on.
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    pub entries: usize,
    pub bytes: u64,
    pub oldest_ms: Option<u64>,
    pub newest_ms: Option<u64>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A directory of entries. Small by design (hundreds), so lookups scan rather than index.
pub struct Store {
    dir: PathBuf,
    ttl_secs: u64,
    max_entries: usize,
}

fn has_error_event(events: &[Value]) -> bool {
    events.iter().any(|e| {
        e.get("type").and_then(|t| t.as_str()) == Some("error")
            || e.get("error").map(|x| !x.is_null()).unwrap_or(false)
    })
}

fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Store {
    pub fn open(dir: impl AsRef<Path>, ttl_secs: u64, max_entries: usize) -> Result<Store, String> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| format!("replay dir {}: {e}", dir.display()))?;
        Ok(Store {
            dir,
            ttl_secs,
            max_entries: max_entries.max(1),
        })
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    fn expired(&self, e: &Entry, now: u64) -> bool {
        now.saturating_sub(e.created_ms) > self.ttl_secs.saturating_mul(1000)
    }

    fn read(&self, path: &Path) -> Option<Entry> {
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    /// The entry for a key, or `None` when absent or expired (an expired file is removed).
    pub fn get(&self, key: &str) -> Option<Entry> {
        self.get_at(key, now_ms())
    }

    pub fn get_at(&self, key: &str, now: u64) -> Option<Entry> {
        if !safe_key(key) {
            return None;
        }
        let p = self.path(key);
        let e = self.read(&p)?;
        if self.expired(&e, now) {
            let _ = fs::remove_file(&p);
            return None;
        }
        Some(e)
    }

    /// Store an entry. Refused: an empty events list, or any error event (replaying a failure would make it permanent).
    pub fn put(&self, entry: &Entry) -> Result<(), String> {
        if !safe_key(&entry.key) {
            return Err("replay: unsafe key".into());
        }
        if entry.events.is_empty() {
            return Err("replay: refusing an entry with no events".into());
        }
        if has_error_event(&entry.events) {
            return Err("replay: refusing an entry that contains an error event".into());
        }
        let bytes = serde_json::to_vec(entry).map_err(|e| e.to_string())?;
        let tmp = self
            .dir
            .join(format!(".{}.tmp{}", entry.key, std::process::id()));
        write_private(&tmp, &bytes).map_err(|e| format!("replay write: {e}"))?;
        fs::rename(&tmp, self.path(&entry.key)).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("replay rename: {e}")
        })?;
        self.evict();
        Ok(())
    }

    fn all(&self) -> Vec<(PathBuf, Entry)> {
        let mut v = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for f in rd.flatten() {
                let p = f.path();
                if p.extension().and_then(|x| x.to_str()) == Some("json") {
                    if let Some(e) = self.read(&p) {
                        v.push((p, e));
                    }
                }
            }
        }
        v
    }

    fn evict(&self) {
        let mut all = self.all();
        if all.len() <= self.max_entries {
            return;
        }
        all.sort_by(|a, b| {
            a.1.created_ms
                .cmp(&b.1.created_ms)
                .then_with(|| a.1.key.cmp(&b.1.key))
        });
        let extra = all.len() - self.max_entries;
        for (p, _) in all.into_iter().take(extra) {
            let _ = fs::remove_file(p);
        }
    }

    /// Remove expired entries; returns how many.
    pub fn purge_expired(&self) -> usize {
        let now = now_ms();
        let mut n = 0;
        for (p, e) in self.all() {
            if self.expired(&e, now) && fs::remove_file(p).is_ok() {
                n += 1;
            }
        }
        n
    }

    pub fn stats(&self) -> StoreStats {
        let mut s = StoreStats::default();
        for (p, e) in self.all() {
            s.entries += 1;
            s.bytes += fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            s.oldest_ms = Some(s.oldest_ms.map_or(e.created_ms, |o| o.min(e.created_ms)));
            s.newest_ms = Some(s.newest_ms.map_or(e.created_ms, |o| o.max(e.created_ms)));
        }
        s
    }

    /// The best stored answer for the same provider and model whose question embedding is at least `threshold` close to
    /// `text`'s. The caller must mark the answer as replayed (it is a near miss, not the same question).
    pub fn find_semantic(
        &self,
        provider: &str,
        model: &str,
        text: &str,
        embedder: &dyn Embedder,
        threshold: f32,
    ) -> Option<(Entry, f32)> {
        let q = embedder.embed(text);
        let now = now_ms();
        let mut best: Option<(Entry, f32)> = None;
        for (_, e) in self.all() {
            if e.provider != provider || e.model != model || self.expired(&e, now) {
                continue;
            }
            let Some(emb) = &e.embedding else { continue };
            let c = cosine(&q, emb);
            if c >= threshold
                && best
                    .as_ref()
                    .map_or(true, |(b, bc)| c > *bc || (c == *bc && e.key < b.key))
            {
                best = Some((e, c));
            }
        }
        best
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::write(path, bytes)
}

/// What a request may use from the cache.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Policy {
    Off,
    ExactOnly,
    ExactAndSemantic { threshold: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Exact,
    Semantic,
    No,
}

pub fn allowed(policy: Policy, request: &Value) -> Decision {
    match policy {
        Policy::Off => Decision::No,
        Policy::ExactOnly => Decision::Exact,
        Policy::ExactAndSemantic { .. } => {
            if looks_like_question(request) {
                Decision::Semantic
            } else {
                Decision::Exact
            }
        }
    }
}

fn block_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| match b {
                Value::String(s) => Some(s.clone()),
                _ => b.get("text").and_then(|t| t.as_str()).map(String::from),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn has_tool_blocks(request: &Value) -> bool {
    let Some(msgs) = request.get("messages").and_then(|m| m.as_array()) else {
        return false;
    };
    msgs.iter().any(|m| {
        m.get("role").and_then(|r| r.as_str()) == Some("tool")
            || m.get("tool_calls").map(|t| !t.is_null()).unwrap_or(false)
            || m.get("content")
                .and_then(|c| c.as_array())
                .map_or(false, |a| {
                    a.iter().any(|b| {
                        matches!(
                            b.get("type").and_then(|t| t.as_str()),
                            Some("tool_use") | Some("tool_result")
                        )
                    })
                })
    })
}

/// The text of the last user message of a request (Anthropic or OpenAI chat shape).
pub fn last_user_text(request: &Value) -> Option<String> {
    let msgs = request.get("messages")?.as_array()?;
    let m = msgs
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))?;
    Some(block_text(m.get("content")?))
}

fn path_like() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)(?:[\w.~-]+[/\\])+[\w.-]+|\b[\w-]+\.(?:rs|js|ts|tsx|jsx|py|json|toml|md|go|c|cpp|h|java|yaml|yml|sh|html|css)\b").unwrap()
    })
}

/// True only for a plain question: the last user message ends with `?` or opens with who / what / why / how / when /
/// where / which / explain / describe / summarize, AND the request has no tool_use or tool_result blocks, offers no
/// tools, and the text names no file path and has no code fence. Anything else is an action and never goes semantic.
pub fn looks_like_question(request: &Value) -> bool {
    if has_tool_blocks(request) {
        return false;
    }
    if request.get("tools").map_or(false, |t| {
        !t.is_null() && t.as_array().map_or(true, |a| !a.is_empty())
    }) {
        return false;
    }
    let Some(text) = last_user_text(request) else {
        return false;
    };
    let t = text.trim();
    if t.is_empty() || t.contains("```") || path_like().is_match(t) {
        return false;
    }
    let first = t
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| !w.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();
    t.ends_with('?')
        || [
            "who",
            "what",
            "why",
            "how",
            "when",
            "where",
            "which",
            "explain",
            "describe",
            "summarize",
            "summarise",
        ]
        .contains(&first.as_str())
}

/// Turns text into a vector. [`HashEmbedder`] is the built-in, dependency-free one; a model-based embedder (fastembed
/// over ONNX, or a local embeddings endpoint) can implement this trait later without touching the store.
pub trait Embedder {
    fn embed(&self, text: &str) -> Vec<f32>;
    fn name(&self) -> &str;
}

/// Hashing-trick bag of words and character trigrams, L2-normalised. Deterministic, no model download. It catches
/// near-identical wording, not deep paraphrase; that is the intended trade for a safe default.
pub struct HashEmbedder {
    dims: usize,
}

impl HashEmbedder {
    pub fn new(dims: usize) -> HashEmbedder {
        HashEmbedder { dims: dims.max(8) }
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        HashEmbedder::new(256)
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

impl Embedder for HashEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dims];
        let lower = text.to_lowercase();
        let mut add = |tok: &str| {
            let h = fnv1a(tok);
            let idx = (h % self.dims as u64) as usize;
            let sign = if (h >> 40) & 1 == 1 { 1.0 } else { -1.0 };
            v[idx] += sign;
        };
        for w in lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            add(w);
            let chars: Vec<char> = w.chars().collect();
            for win in chars.windows(3) {
                let tri: String = win.iter().collect();
                add(&format!("#{tri}"));
            }
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
        v
    }
    fn name(&self) -> &str {
        "hash-trigram"
    }
}

/// Cosine similarity; 0 for mismatched lengths or a zero vector.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Tmp {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "pgm-replay-{}-{}-{}",
                std::process::id(),
                now_ms(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(key: &str, created: u64, text: Option<&str>) -> Entry {
        Entry {
            key: key.into(),
            provider: "p".into(),
            model: "m".into(),
            created_ms: created,
            events: vec![json!({"type":"text","text":"hi"})],
            usage: None,
            prompt_hash: "h".into(),
            kind: EntryKind::Exact,
            embedding: text.map(|t| HashEmbedder::default().embed(t)),
        }
    }

    #[test]
    fn key_ignores_order_and_volatile_keys() {
        let a = json!({"model":"m","messages":[{"role":"user","content":"hi","cache_control":{"type":"ephemeral"}}],"stream":true,"metadata":{"u":1},"x-trace":"a"});
        let b = json!({"x-trace":"b","messages":[{"content":"hi","role":"user"}],"model":"m","user":"bob","session_id":"s"});
        assert_eq!(canonical_key("p", "m", &a), canonical_key("p", "m", &b));
    }

    #[test]
    fn key_differs_on_content_provider_model_and_order() {
        let a = json!({"messages":[{"role":"user","content":"hi"}]});
        let b = json!({"messages":[{"role":"user","content":"hi!"}]});
        assert_ne!(canonical_key("p", "m", &a), canonical_key("p", "m", &b));
        assert_ne!(canonical_key("p", "m", &a), canonical_key("q", "m", &a));
        assert_ne!(canonical_key("p", "m", &a), canonical_key("p", "n", &a));
        let c = json!({"l":[1,2]});
        let d = json!({"l":[2,1]});
        assert_ne!(canonical_key("p", "m", &c), canonical_key("p", "m", &d));
    }

    #[test]
    fn put_get_roundtrip() {
        let t = Tmp::new();
        let s = Store::open(&t.0, 3600, 10).unwrap();
        let e = entry("abc", now_ms(), None);
        s.put(&e).unwrap();
        assert_eq!(s.get("abc"), Some(e));
        assert!(s.get("missing").is_none());
        assert!(s.get("../etc/passwd").is_none());
        assert_eq!(s.stats().entries, 1);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = Tmp::new();
        let s = Store::open(&t.0, 3600, 10).unwrap();
        s.put(&entry("abc", now_ms(), None)).unwrap();
        let m = fs::metadata(t.0.join("abc.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(m & 0o777, 0o600);
    }

    #[test]
    fn expired_entries_vanish() {
        let t = Tmp::new();
        let s = Store::open(&t.0, 10, 10).unwrap();
        s.put(&entry("old", 0, None)).unwrap();
        s.put(&entry("new", now_ms(), None)).unwrap();
        assert!(s.get("old").is_none());
        assert!(!t.0.join("old.json").exists());
        assert!(s.get("new").is_some());
        s.put(&entry("old2", 1, None)).unwrap();
        assert_eq!(s.purge_expired(), 1);
    }

    #[test]
    fn errors_and_empty_are_refused() {
        let t = Tmp::new();
        let s = Store::open(&t.0, 3600, 10).unwrap();
        let mut e = entry("bad", now_ms(), None);
        e.events = vec![
            json!({"type":"text"}),
            json!({"type":"error","error":{"message":"overloaded"}}),
        ];
        assert!(s.put(&e).is_err());
        e.events = vec![json!({"error":{"message":"x"}})];
        assert!(s.put(&e).is_err());
        e.events = vec![];
        assert!(s.put(&e).is_err());
        assert_eq!(s.stats().entries, 0);
    }

    #[test]
    fn the_oldest_are_evicted() {
        let t = Tmp::new();
        let s = Store::open(&t.0, 3600, 2).unwrap();
        let n = now_ms();
        s.put(&entry("a", n - 3, None)).unwrap();
        s.put(&entry("b", n - 2, None)).unwrap();
        s.put(&entry("c", n - 1, None)).unwrap();
        assert!(s.get("a").is_none());
        assert!(s.get("b").is_some() && s.get("c").is_some());
        let st = s.stats();
        assert_eq!(
            (st.entries, st.oldest_ms, st.newest_ms),
            (2, Some(n - 2), Some(n - 1))
        );
    }

    #[test]
    fn policy_decisions() {
        let q = json!({"messages":[{"role":"user","content":"What is a mutex?"}]});
        let edit =
            json!({"messages":[{"role":"user","content":"Rename the function in src/main.rs"}]});
        let sem = Policy::ExactAndSemantic { threshold: 0.9 };
        assert_eq!(allowed(Policy::Off, &q), Decision::No);
        assert_eq!(allowed(Policy::ExactOnly, &q), Decision::Exact);
        assert_eq!(allowed(sem, &q), Decision::Semantic);
        assert_eq!(allowed(sem, &edit), Decision::Exact);
    }

    #[test]
    fn an_edit_or_tool_request_is_never_semantic() {
        let sem = Policy::ExactAndSemantic { threshold: 0.9 };
        let with_tools = json!({"tools":[{"name":"edit"}],"messages":[{"role":"user","content":"What is this?"}]});
        let tool_block = json!({"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"1","name":"x","input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"1","content":"ok"},{"type":"text","text":"why?"}]}]});
        let fence = json!({"messages":[{"role":"user","content":"What does this do?\n```\nfn x(){}\n```"}]});
        let path = json!({"messages":[{"role":"user","content":"What is in lib.rs?"}]});
        let path2 = json!({"messages":[{"role":"user","content":"Explain crates/a/src/b.txt"}]});
        let cmd = json!({"messages":[{"role":"user","content":"Add a test for the parser"}]});
        for r in [&with_tools, &tool_block, &fence, &path, &path2, &cmd] {
            assert_eq!(allowed(sem, r), Decision::Exact, "{r}");
        }
        let empty_tools = json!({"tools":[],"messages":[{"role":"user","content":[{"type":"text","text":"Explain borrowing"}]}]});
        assert_eq!(allowed(sem, &empty_tools), Decision::Semantic);
    }

    #[test]
    fn semantic_lookup_hits_near_wording_and_misses_others() {
        let t = Tmp::new();
        let s = Store::open(&t.0, 3600, 10).unwrap();
        let emb = HashEmbedder::default();
        s.put(&entry(
            "cap",
            now_ms(),
            Some("What is the capital of France?"),
        ))
        .unwrap();
        let hit = s
            .find_semantic("p", "m", "what is the capital city of France", &emb, 0.8)
            .unwrap();
        assert_eq!(hit.0.key, "cap");
        assert!(hit.1 >= 0.8 && hit.1 <= 1.0001, "{}", hit.1);
        assert!(s
            .find_semantic("p", "m", "How do I bake sourdough bread at home", &emb, 0.8)
            .is_none());
        assert!(s
            .find_semantic("p", "other", "What is the capital of France?", &emb, 0.8)
            .is_none());
        assert!(s
            .find_semantic("p", "m", "what is the capital city of France", &emb, 0.9999)
            .is_none());
    }

    #[test]
    fn embedder_is_deterministic_and_normalised() {
        let e = HashEmbedder::default();
        let a = e.embed("hello world");
        assert_eq!(a, e.embed("hello world"));
        assert!((a.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
        assert_eq!(a.len(), 256);
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-4);
        assert_eq!(cosine(&a, &[1.0]), 0.0);
        assert_eq!(e.embed("").iter().sum::<f32>(), 0.0);
    }
}
