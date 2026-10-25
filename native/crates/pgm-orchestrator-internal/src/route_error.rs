//! Route error classification (DECISIONS A1 item 9): what an HTTP status and body from a worker's route mean for
//! the policy. A route refusal benches on the first strike (it reuses `Outcome::Unavailable`, the existing
//! bench-at-once path of `health`); a bare 429 is one strike; 5xx is transient (a strike); a context-length error is
//! the brief's fault, so the worker takes no strike and only a larger window is worth trying.
use crate::health::{Health, Outcome};
use crate::profile::TaskKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// this route will not serve us now (403, not served, rate-limited upstream): bench on the first strike
    Refusal { reason: String },
    /// a plain 429: one strike, not a bench
    RateLimited,
    /// 5xx or no response: try again / another worker
    Transient,
    /// retrying the same brief on a worker of the same or a smaller window is pointless
    Fatal { reason: String },
}

/// Body patterns (lower case) that mean "this specific route refuses", whatever the status. One table.
const REFUSAL_PATTERNS: &[&str] = &[
    "temporarily rate-limited", // OpenRouter free tier: the route is saturated
    "rate-limited upstream",    // the provider behind the router is limiting this model
    "no endpoints found",       // OpenRouter: no provider serves this model right now
    "provider returned error",  // OpenRouter wrapper around an upstream failure for this route
];

/// Body patterns (lower case) that mean the brief does not fit the window (Fatal when the status is 400).
const CONTEXT_PATTERNS: &[&str] = &[
    "context length",
    "context_length",
    "too long",
    "maximum context",
];

pub fn classify_route_error(status: Option<u16>, body: &str) -> RouteError {
    let b = body.to_ascii_lowercase();
    let snippet = || -> String {
        let s: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
        s.chars().take(160).collect()
    };
    if matches!(status, Some(400) | Some(413)) && CONTEXT_PATTERNS.iter().any(|p| b.contains(p)) {
        return RouteError::Fatal {
            reason: format!("context window exceeded: {}", snippet()),
        };
    }
    if status == Some(403) {
        return RouteError::Refusal {
            reason: format!("403 forbidden: {}", snippet()),
        };
    }
    if status == Some(404) {
        return RouteError::Refusal {
            reason: format!("404 route not served: {}", snippet()),
        };
    }
    if let Some(p) = REFUSAL_PATTERNS.iter().find(|p| b.contains(**p)) {
        return RouteError::Refusal {
            reason: format!("{p}: {}", snippet()),
        };
    }
    match status {
        Some(429) => RouteError::RateLimited,
        Some(s) if s >= 500 => RouteError::Transient,
        None => RouteError::Transient,
        Some(s) => RouteError::Fatal {
            reason: format!("HTTP {s}: {}", snippet()),
        },
    }
}

impl RouteError {
    /// The health outcome this error is recorded as: refusals bench at once, 429/5xx are strikes, a context-length
    /// error is nobody's strike (it is the brief's, so `Cancelled`).
    pub fn outcome(&self) -> Outcome {
        match self {
            RouteError::Refusal { .. } => Outcome::Unavailable,
            RouteError::RateLimited | RouteError::Transient => Outcome::Error,
            RouteError::Fatal { reason } if reason.starts_with("context window") => {
                Outcome::Cancelled
            }
            RouteError::Fatal { .. } => Outcome::Error,
        }
    }
    pub fn reason(&self) -> String {
        match self {
            RouteError::Refusal { reason } | RouteError::Fatal { reason } => reason.clone(),
            RouteError::RateLimited => "429 rate limited".into(),
            RouteError::Transient => "transient server/network error".into(),
        }
    }
}

/// The window a retry needs when the error says how many tokens were requested ("however you requested 12000
/// tokens", "resulted in 12000 tokens"); None when the body does not say. A larger window is worth trying, a
/// smaller or equal one is not.
pub fn needs_context_at_least(body: &str) -> Option<u32> {
    let b = body.to_ascii_lowercase();
    for key in ["you requested ", "resulted in ", "requested "] {
        if let Some(i) = b.find(key) {
            let digits: String = b[i + key.len()..]
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .filter(|c| *c != ',')
                .collect();
            if let Ok(n) = digits.parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

impl Health {
    /// Record a classified route error through the existing `record` (so a refusal benches on the first strike, a
    /// 429 takes one strike, and a context-length error takes none).
    pub fn record_route_error(
        &mut self,
        route: &str,
        kind: TaskKind,
        err: &RouteError,
        now: u64,
        failures_before_bench: u32,
        bench_secs: u64,
    ) {
        self.record(
            route,
            kind,
            err.outcome(),
            now,
            failures_before_bench,
            bench_secs,
            &err.reason(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_refusal(e: &RouteError) -> bool {
        matches!(e, RouteError::Refusal { .. })
    }

    #[test]
    fn the_status_and_body_table() {
        let cases: &[(Option<u16>, &str, &str)] = &[
            (Some(403), "only available on agentic harnesses", "refusal"),
            (Some(404), "", "refusal"),
            (Some(429), "slow down", "rate"),
            (
                Some(429),
                "model is temporarily rate-limited, retry shortly",
                "refusal",
            ),
            (Some(200), "Provider returned error", "refusal"),
            (Some(404), "No endpoints found for x/y", "refusal"),
            (Some(502), "bad gateway", "transient"),
            (Some(500), "", "transient"),
            (None, "connection reset", "transient"),
            (
                Some(400),
                "This model's maximum context length is 8192 tokens",
                "fatal",
            ),
            (Some(400), "prompt is too long", "fatal"),
            (Some(400), "invalid json", "fatal"),
            (Some(401), "bad key", "fatal"),
        ];
        for (status, body, want) in cases {
            let got = classify_route_error(*status, body);
            let ok = match *want {
                "refusal" => is_refusal(&got),
                "rate" => got == RouteError::RateLimited,
                "transient" => got == RouteError::Transient,
                _ => matches!(got, RouteError::Fatal { .. }),
            };
            assert!(ok, "{status:?} {body:?} -> {got:?}, wanted {want}");
        }
    }

    #[test]
    fn a_refusal_benches_on_the_first_strike_even_with_two_strikes_allowed() {
        let mut h = Health::new();
        let e = classify_route_error(Some(403), "forbidden");
        h.record_route_error("w", TaskKind::General, &e, 100, 2, 60);
        assert!(h.benched("w", 101) && h.of("w").strikes == 1);
        assert!(
            !h.refuses("w", TaskKind::General),
            "a route refusal is not a content refusal: not permanent per kind"
        );
    }

    #[test]
    fn a_429_strikes_only_and_benches_at_the_threshold() {
        let mut h = Health::new();
        let e = classify_route_error(Some(429), "slow down");
        h.record_route_error("w", TaskKind::General, &e, 100, 2, 60);
        assert!(!h.benched("w", 101) && h.of("w").strikes == 1);
        h.record_route_error("w", TaskKind::General, &e, 102, 2, 60);
        assert!(h.benched("w", 103));
    }

    #[test]
    fn a_context_length_error_is_nobodys_strike() {
        let mut h = Health::new();
        let e = classify_route_error(Some(400), "context length exceeded");
        assert!(matches!(e, RouteError::Fatal { .. }));
        h.started("w");
        h.record_route_error("w", TaskKind::General, &e, 100, 1, 60);
        assert!(!h.benched("w", 101) && h.of("w").strikes == 0 && h.load("w") == 0);
    }

    #[test]
    fn transient_errors_are_strikes() {
        let mut h = Health::new();
        h.record_route_error(
            "w",
            TaskKind::General,
            &classify_route_error(Some(503), ""),
            100,
            2,
            60,
        );
        assert_eq!(h.of("w").strikes, 1);
        assert!(!h.benched("w", 101));
    }

    #[test]
    fn the_requested_window_is_read_from_the_body() {
        assert_eq!(
            needs_context_at_least(
                "maximum context length is 8192 tokens, however you requested 12,345 tokens"
            ),
            Some(12345)
        );
        assert_eq!(
            needs_context_at_least("this request resulted in 9000 tokens"),
            Some(9000)
        );
        assert_eq!(needs_context_at_least("prompt is too long"), None);
    }
}
