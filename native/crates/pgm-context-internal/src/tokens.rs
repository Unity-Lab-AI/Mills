//! Token counting per provider family. Every other layer's "saved" number rests on this, so a counter says whether it
//! is exact (a real BPE) or an estimate (characters per token, calibrated when samples are given).
use std::sync::OnceLock;

pub trait Counter: Send + Sync {
    fn count(&self, text: &str) -> usize;
    /// The tokenizer family: "o200k_base", "cl100k_base", "anthropic-estimate", "estimate".
    fn family(&self) -> &'static str;
    /// True for a real tokenizer, false for an estimator.
    fn exact(&self) -> bool;
}

/// A characters-per-token estimator (Anthropic publishes no tokenizer; local models vary). The default ratio is
/// calibrated on English prose and code mixes; `calibrate` moves it toward measured samples.
pub struct Estimator { family: &'static str, chars_per_token: f64 }

impl Estimator {
    pub fn new(family: &'static str, chars_per_token: f64) -> Estimator { Estimator { family, chars_per_token: chars_per_token.max(1.0) } }
    pub fn anthropic() -> Estimator { Estimator::new("anthropic-estimate", 3.6) }
    pub fn generic() -> Estimator { Estimator::new("estimate", 4.0) }
    pub fn chars_per_token(&self) -> f64 { self.chars_per_token }
    /// Fit the ratio to measured (text, tokens) samples: the total characters over the total tokens, so long samples
    /// weigh more. Samples with zero tokens are ignored.
    pub fn calibrate(&mut self, samples: &[(&str, usize)]) {
        let (chars, toks) = samples.iter().filter(|(_, t)| *t > 0).fold((0usize, 0usize), |(c, t), (s, n)| (c + s.chars().count(), t + n));
        if toks > 0 { self.chars_per_token = (chars as f64 / toks as f64).max(1.0); }
    }
}

impl Counter for Estimator {
    fn count(&self, text: &str) -> usize {
        if text.is_empty() { return 0; }
        // code and punctuation-heavy text tokenizes denser than prose: count non-space runs as a floor
        let chars = text.chars().count() as f64;
        let words = text.split_whitespace().count() as f64;
        ((chars / self.chars_per_token).max(words * 0.75)).ceil() as usize
    }
    fn family(&self) -> &'static str { self.family }
    fn exact(&self) -> bool { false }
}

/// tiktoken's encodings, loaded once per process (loading takes a moment).
pub struct Tiktoken { family: &'static str }
static O200K: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();
static CL100K: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();
impl Tiktoken {
    pub fn o200k() -> Tiktoken { Tiktoken { family: "o200k_base" } }
    pub fn cl100k() -> Tiktoken { Tiktoken { family: "cl100k_base" } }
    fn bpe(&self) -> Option<&'static tiktoken_rs::CoreBPE> {
        match self.family {
            "o200k_base" => O200K.get_or_init(|| tiktoken_rs::o200k_base().ok()).as_ref(),
            _ => CL100K.get_or_init(|| tiktoken_rs::cl100k_base().ok()).as_ref(),
        }
    }
}
impl Counter for Tiktoken {
    fn count(&self, text: &str) -> usize { match self.bpe() { Some(b) => b.encode_with_special_tokens(text).len(), None => Estimator::generic().count(text) } }
    fn family(&self) -> &'static str { self.family }
    fn exact(&self) -> bool { self.bpe().is_some() }
}

/// The counter for a model id: OpenAI's newer families (gpt-4o, gpt-5.x, o-series) → o200k; older GPT-4 / 3.5 →
/// cl100k; Claude → the Anthropic estimator; anything else (Gemini, local, OpenRouter ids with a vendor prefix that is
/// one of the above is unwrapped) → the generic estimator.
pub fn for_model(model: &str) -> Box<dyn Counter> {
    let m = model.trim().to_ascii_lowercase();
    let bare = m.rsplit('/').next().unwrap_or(&m).split(':').next().unwrap_or(&m).to_string();
    if bare.starts_with("claude") { return Box::new(Estimator::anthropic()); }
    if bare.starts_with("gpt-4o") || bare.starts_with("gpt-5") || bare.starts_with("gpt-4.1") || bare.starts_with("o1") || bare.starts_with("o3") || bare.starts_with("o4") || bare.starts_with("chatgpt") { return Box::new(Tiktoken::o200k()); }
    if bare.starts_with("gpt-4") || bare.starts_with("gpt-3.5") || bare.starts_with("text-embedding") { return Box::new(Tiktoken::cl100k()); }
    Box::new(Estimator::generic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_follow_the_model_id() {
        assert_eq!(for_model("claude-haiku-5-5").family(), "anthropic-estimate");
        assert_eq!(for_model("anthropic/claude-sonnet-5-5:free").family(), "anthropic-estimate");
        assert_eq!(for_model("gpt-5.5").family(), "o200k_base");
        assert_eq!(for_model("openai/gpt-4o-mini").family(), "o200k_base");
        assert_eq!(for_model("gpt-4-turbo").family(), "cl100k_base");
        assert_eq!(for_model("qwen3-coder:30b").family(), "estimate");
    }

    #[test]
    fn tiktoken_counts_exactly_when_the_encoding_loads() {
        let c = Tiktoken::o200k();
        let n = c.count("hello world, this is a token count");
        if c.exact() { assert!(n >= 6 && n <= 12, "{n}"); } else { assert!(n > 0); }
        assert_eq!(c.count(""), 0);
    }

    #[test]
    fn the_estimator_calibrates_toward_samples() {
        let mut e = Estimator::anthropic();
        let before = e.count("fn main() { println!(\"hi\"); }");
        e.calibrate(&[("aaaaaaaaaa", 5), ("bbbbbbbbbb", 5)]); // 2 chars per token measured
        assert!((e.chars_per_token() - 2.0).abs() < 1e-9);
        assert!(e.count("fn main() { println!(\"hi\"); }") > before);
        assert!(!e.exact());
        e.calibrate(&[("x", 0)]); // ignored
        assert!((e.chars_per_token() - 2.0).abs() < 1e-9);
    }
}
