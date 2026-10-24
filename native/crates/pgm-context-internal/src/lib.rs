//! **pgm-context-internal** = PGM_Context_Internal (PsyGraphMediaContextInternal): fewer tokens and more cache hits,
//! for every model provider, without taking anything away from what the model is told.
//!
//! The design is the team note / Jcode-Mods `TOKENS.md`. The first layers, all without model inference:
//! - [`tokens`]: counting per provider family (exact BPE for the OpenAI families through tiktoken, a calibrated
//!   estimator for Anthropic and local models, honest about which it is)
//! - [`ledger`]: a usage ledger from the usage objects providers return (Anthropic, OpenAI, Gemini, OpenRouter
//!   shapes): paid vs cached input per turn, per session, per provider; hit ratio; cost with a price table
//! - [`assembly`]: a prompt as ordered segments with a stability class; the planner orders the most stable first,
//!   hashes each, places cache breakpoints per provider (Anthropic: at most 4, each big enough), and a lint refuses
//!   volatile text ("3 min ago", timestamps, random ids) inside a stable segment, naming the segment to move
//! - [`shape`]: tool output shaped before it enters the context: ANSI and trailing whitespace gone, repeated lines
//!   folded, a file read again unchanged becomes a one-line reference, long output capped with a stash handle that
//!   brings any part back (reversible, never silently lost)
//!
//! - [`report`]: ledger lines that carry per-tool shaping savings (`TurnRecord`, old lines still parse) and the
//!   `SavingsReport` for `/context report`: per tool, per session, cache hit ratio, totals; text and JSON
//! - [`retrieve`]: the retrieval filter, OFF by default: scores earlier chunks against the latest user message and
//!   keeps the best within a budget (pinned and newest turns always, tool_use / tool_result pairs never split); a
//!   Shadow mode records what it would have saved and how often it would have been wrong (`AbSummary`); BM25 behind a
//!   `Ranker` trait; a sticky drop policy (`DropPolicy`, `DropLedger`) that keeps applying the filter from breaking
//!   the prefix cache; the structured shapers of [`shape`] (cargo, test runners, git diff / status, JSON, listings)
//! - [`simulate`]: the offline cache simulator: replays recorded requests through a provider's caching rules and
//!   reports cached vs paid tokens, before and after shaping / planning / moving volatile text into the last message
//!
//! - [`repomap`]: a source tree as symbols with signatures, not bodies, ranked by how often other files mention them
//!   and fitted to a token budget; byte-stable for an unchanged tree so it caches as a prefix
//! - [`replay`]: an exact-match response cache keyed by (provider, model, canonical request), plus an opt-in semantic
//!   near-miss lookup that is allowed for plain questions only
//! - [`route`]: prefix-cache locality for local and shared backends: a session stays on the backend that holds its
//!   prefix; vLLM `/metrics` parsing
//!
//! Each arrives behind a switch with the ledger measuring it first.

pub mod assembly;
pub mod ledger;
pub mod replay;
pub mod repomap;
pub mod report;
pub mod retrieve;
pub mod route;
pub mod shape;
pub mod simulate;
pub mod tokens;

/// A short content hash (SHA-256, first 12 hex chars): enough to say "the same bytes", never used as a secret.
pub fn short_hash(text: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(text.as_bytes());
    hex::encode(&d[..6])
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_hash_is_stable_and_short() {
        assert_eq!(super::short_hash("abc"), super::short_hash("abc"));
        assert_ne!(super::short_hash("abc"), super::short_hash("abd"));
        assert_eq!(super::short_hash("abc").len(), 12);
    }
}
