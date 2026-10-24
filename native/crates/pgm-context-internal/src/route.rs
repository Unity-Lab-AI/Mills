//! Prefix-cache routing for local and shared backends: a session keeps going to the backend that already holds its
//! prefix in the KV cache, because a warm prefix is read at a fraction of the cost (and time) of a cold one. The router
//! is plain memory with a TTL; it makes no network calls. The caller reports health and load through [`Backend`].
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One place a model can run (a vLLM, llama.cpp or Ollama server, or a shared gateway).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Backend {
    pub id: String,
    pub url: String,
    pub model: String,
    pub healthy: bool,
    pub inflight: u32,
    /// The backend's own prefix-cache hit rate when it reports one (see [`parse_vllm_metrics`]).
    pub prefix_hit_rate: Option<f32>,
    pub last_seen_ms: u64,
}

/// Why a backend was chosen, so the ledger and the UI can say it in plain words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    /// The session already lives there.
    Sticky,
    /// Another session with the same prefix was served there, so the prefix is probably warm.
    PrefixKnown,
    /// No locality to use: the least busy healthy backend.
    LeastLoaded,
    /// Only one healthy candidate existed.
    OnlyOne,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pick {
    pub backend_id: String,
    pub reason: Reason,
}

#[derive(Clone, Debug)]
struct Sticky {
    backend_id: String,
    prefix_hash: String,
    last_ms: u64,
}

/// Session stickiness plus a `prefix_hash -> backend` memory (LMCache-style sharing without a shared cache: a second
/// session with the same system prompt and tools goes where the first one warmed it).
pub struct Router {
    sticky: HashMap<String, Sticky>,
    prefixes: HashMap<String, (String, u64)>,
    ttl_secs: u64,
}

impl Router {
    pub fn new(ttl_secs: u64) -> Router {
        Router {
            sticky: HashMap::new(),
            prefixes: HashMap::new(),
            ttl_secs,
        }
    }

    fn fresh(&self, last_ms: u64, now_ms: u64) -> bool {
        now_ms.saturating_sub(last_ms) <= self.ttl_secs.saturating_mul(1000)
    }

    /// Choose a backend for this session and remember the choice. Rules in order: the session's sticky backend while it
    /// is a healthy candidate and not expired; a healthy backend known to hold the same prefix; the least loaded healthy
    /// candidate (ties by id); a single healthy candidate reports `OnlyOne`. No healthy candidate gives `None`.
    pub fn pick(
        &mut self,
        session: &str,
        prefix_hash: &str,
        candidates: &[Backend],
        now_ms: u64,
    ) -> Option<Pick> {
        let healthy: Vec<&Backend> = candidates.iter().filter(|b| b.healthy).collect();
        if healthy.is_empty() {
            return None;
        }
        let is_healthy = |id: &str| healthy.iter().any(|b| b.id == id);
        let mut chosen: Option<Pick> = None;
        if let Some(s) = self.sticky.get(session) {
            if self.fresh(s.last_ms, now_ms) && is_healthy(&s.backend_id) {
                chosen = Some(Pick {
                    backend_id: s.backend_id.clone(),
                    reason: Reason::Sticky,
                });
            }
        }
        if chosen.is_none() {
            if let Some((id, seen)) = self.prefixes.get(prefix_hash) {
                if self.fresh(*seen, now_ms) && is_healthy(id) {
                    chosen = Some(Pick {
                        backend_id: id.clone(),
                        reason: Reason::PrefixKnown,
                    });
                }
            }
        }
        let pick = chosen.unwrap_or_else(|| {
            let best = healthy
                .iter()
                .min_by(|a, b| a.inflight.cmp(&b.inflight).then_with(|| a.id.cmp(&b.id)))
                .expect("non-empty");
            let reason = if healthy.len() == 1 {
                Reason::OnlyOne
            } else {
                Reason::LeastLoaded
            };
            Pick {
                backend_id: best.id.clone(),
                reason,
            }
        });
        self.sticky.insert(
            session.to_string(),
            Sticky {
                backend_id: pick.backend_id.clone(),
                prefix_hash: prefix_hash.to_string(),
                last_ms: now_ms,
            },
        );
        self.prefixes
            .insert(prefix_hash.to_string(), (pick.backend_id.clone(), now_ms));
        Some(pick)
    }

    /// Drop a session's stickiness (the session ended).
    pub fn forget(&mut self, session: &str) {
        self.sticky.remove(session);
    }

    /// Drop everything older than the TTL.
    pub fn prune(&mut self, now_ms: u64) {
        let ttl = self.ttl_secs.saturating_mul(1000);
        self.sticky
            .retain(|_, s| now_ms.saturating_sub(s.last_ms) <= ttl);
        self.prefixes
            .retain(|_, (_, t)| now_ms.saturating_sub(*t) <= ttl);
    }

    pub fn sessions(&self) -> usize {
        self.sticky.len()
    }

    /// The prefix a session was last routed with (to notice a prefix change after a model switch).
    pub fn prefix_of(&self, session: &str) -> Option<&str> {
        self.sticky.get(session).map(|s| s.prefix_hash.as_str())
    }
}

/// A description of a cheap warm-up request (one output token) that loads a prefix into a backend's cache. It is only a
/// hint: the caller may send it after a model switch or a backend change, so the first real turn is not the cold one.
pub fn warm_hint(backend: &Backend, prefix_hash: &str) -> serde_json::Value {
    serde_json::json!({ "backend": backend.id, "prefix_hash": prefix_hash, "max_tokens": 1 })
}

/// What a vLLM (or llama.cpp) `/metrics` page says about prefix caching and load.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VllmMetrics {
    pub prefix_cache_hits: f64,
    pub prefix_cache_queries: f64,
    pub hit_rate: Option<f32>,
    pub running: f64,
    pub waiting: f64,
}

/// Parse Prometheus text. Counters are summed over their label sets. The hit rate is the gauge when the build exposes
/// one, else hits over queries. `None` when no line of interest was found.
pub fn parse_vllm_metrics(text: &str) -> Option<VllmMetrics> {
    let mut m = VllmMetrics::default();
    let mut found = false;
    let mut gauge: Option<f64> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name_part, value_part) = match line.rfind(|c: char| c.is_whitespace()) {
            Some(i) => (line[..i].trim(), line[i..].trim()),
            None => continue,
        };
        let name = name_part.split('{').next().unwrap_or("").trim();
        let Ok(v) = value_part.parse::<f64>() else {
            continue;
        };
        if !v.is_finite() {
            continue;
        }
        let key = name
            .strip_prefix("vllm:")
            .or_else(|| name.strip_prefix("llamacpp:"));
        let Some(key) = key else { continue };
        match key {
            "prefix_cache_hits_total" | "prefix_cache_hits" => {
                m.prefix_cache_hits += v;
                found = true;
            }
            "prefix_cache_queries_total" | "prefix_cache_queries" => {
                m.prefix_cache_queries += v;
                found = true;
            }
            "gpu_prefix_cache_hit_rate" | "prefix_cache_hit_rate" => {
                gauge = Some(v);
                found = true;
            }
            "num_requests_running" | "requests_processing" => {
                m.running += v;
                found = true;
            }
            "num_requests_waiting" | "requests_deferred" => {
                m.waiting += v;
                found = true;
            }
            _ => {}
        }
    }
    if !found {
        return None;
    }
    m.hit_rate = match gauge {
        Some(g) => Some(g as f32),
        None if m.prefix_cache_queries > 0.0 => {
            Some((m.prefix_cache_hits / m.prefix_cache_queries) as f32)
        }
        None => None,
    };
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be(id: &str, healthy: bool, inflight: u32) -> Backend {
        Backend {
            id: id.into(),
            url: format!("http://{id}:8000"),
            model: "m".into(),
            healthy,
            inflight,
            prefix_hit_rate: None,
            last_seen_ms: 0,
        }
    }

    #[test]
    fn stickiness_survives_while_healthy() {
        let mut r = Router::new(600);
        let c = vec![be("a", true, 5), be("b", true, 0)];
        assert_eq!(r.pick("s1", "p", &c, 1000).unwrap().backend_id, "b");
        let c2 = vec![be("a", true, 0), be("b", true, 9)];
        let p = r.pick("s1", "p", &c2, 2000).unwrap();
        assert_eq!((p.backend_id.as_str(), p.reason), ("b", Reason::Sticky));
    }

    #[test]
    fn it_moves_when_the_sticky_backend_is_unhealthy() {
        let mut r = Router::new(600);
        r.pick("s1", "p", &[be("a", true, 0), be("b", true, 1)], 0);
        let p = r
            .pick("s1", "p", &[be("a", false, 0), be("b", true, 1)], 10)
            .unwrap();
        assert_eq!(p.backend_id, "b");
        assert_eq!(p.reason, Reason::OnlyOne);
    }

    #[test]
    fn a_known_prefix_is_shared_across_sessions() {
        let mut r = Router::new(600);
        r.pick("s1", "sys", &[be("a", true, 0), be("b", true, 5)], 0);
        let p = r
            .pick("s2", "sys", &[be("a", true, 9), be("b", true, 0)], 5)
            .unwrap();
        assert_eq!(
            (p.backend_id.as_str(), p.reason),
            ("a", Reason::PrefixKnown)
        );
    }

    #[test]
    fn least_loaded_ties_break_by_id() {
        let mut r = Router::new(600);
        let p = r
            .pick(
                "s",
                "x",
                &[be("b", true, 2), be("a", true, 2), be("c", true, 7)],
                0,
            )
            .unwrap();
        assert_eq!(
            (p.backend_id.as_str(), p.reason),
            ("a", Reason::LeastLoaded)
        );
    }

    #[test]
    fn none_when_nothing_is_healthy() {
        let mut r = Router::new(600);
        assert!(r.pick("s", "x", &[be("a", false, 0)], 0).is_none());
        assert!(r.pick("s", "x", &[], 0).is_none());
    }

    #[test]
    fn stickiness_expires() {
        let mut r = Router::new(10);
        r.pick("s", "x", &[be("a", true, 9), be("b", true, 0)], 0);
        let p = r
            .pick("s", "other", &[be("a", true, 0), be("b", true, 9)], 11_000)
            .unwrap();
        assert_eq!(
            (p.backend_id.as_str(), p.reason),
            ("a", Reason::LeastLoaded)
        );
    }

    #[test]
    fn forget_and_prune_drop_state() {
        let mut r = Router::new(10);
        r.pick("s1", "x", &[be("a", true, 0)], 0);
        r.pick("s2", "y", &[be("a", true, 0)], 9_000);
        assert_eq!(r.sessions(), 2);
        r.forget("s1");
        assert_eq!(r.sessions(), 1);
        r.prune(30_000);
        assert_eq!(r.sessions(), 0);
        assert!(r.prefix_of("s2").is_none());
    }

    #[test]
    fn warm_hint_shape() {
        let h = warm_hint(&be("a", true, 0), "abc");
        assert_eq!(h["backend"], "a");
        assert_eq!(h["prefix_hash"], "abc");
        assert_eq!(h["max_tokens"], 1);
    }

    #[test]
    fn parses_a_vllm_metrics_page() {
        let text = "# HELP vllm:prefix_cache_hits_total hits\n# TYPE vllm:prefix_cache_hits_total counter\n\
vllm:prefix_cache_hits_total{model_name=\"q\",engine=\"0\"} 750\n\
vllm:prefix_cache_hits_total{model_name=\"q\",engine=\"1\"} 250\n\
vllm:prefix_cache_queries_total{model_name=\"q\",engine=\"0\"} 1500\n\
vllm:prefix_cache_queries_total{model_name=\"q\",engine=\"1\"} 500\n\
vllm:num_requests_running{model_name=\"q\"} 3\n\
vllm:num_requests_waiting{model_name=\"q\"} 1\n\
process_cpu_seconds_total 12.5\n";
        let m = parse_vllm_metrics(text).unwrap();
        assert_eq!(m.prefix_cache_hits, 1000.0);
        assert_eq!(m.prefix_cache_queries, 2000.0);
        assert_eq!(m.hit_rate, Some(0.5));
        assert_eq!((m.running, m.waiting), (3.0, 1.0));
    }

    #[test]
    fn the_gauge_wins_when_present() {
        let m = parse_vllm_metrics("vllm:gpu_prefix_cache_hit_rate{model_name=\"q\"} 0.8\nvllm:prefix_cache_hits_total 1\nvllm:prefix_cache_queries_total 10\n").unwrap();
        assert!((m.hit_rate.unwrap() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn garbage_gives_none() {
        assert!(parse_vllm_metrics("").is_none());
        assert!(parse_vllm_metrics("hello world\n\u{0}\u{1}\nfoo{bar} nope\n").is_none());
        assert!(parse_vllm_metrics("vllm:num_requests_running abc\n").is_none());
    }
}
