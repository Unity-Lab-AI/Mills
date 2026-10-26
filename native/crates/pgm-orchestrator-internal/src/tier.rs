//! Capability tiers and the cheapest-first order of routes (Jcode-Mods item 52).
//!
//! "Cheapest first" needs a price. Some routes report none (a Claude OAuth login has no per-route price), and then the
//! old tie-break by name put the most expensive model of a family at the top of a "budget" list. An unknown price is
//! not evidence of being cheap, so an unpriced route sorts after every priced one, and among unpriced routes the
//! order is a guess at capability from the model's own name: small and fast names first, unknown names in the middle
//! (they cannot claim to be cheap), large names last. The guess is only ever a tie-break, never a price.

/// How capable (and so, as a rule, how costly) a model looks from its name alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// small, fast, cheap by family naming: haiku, mini, nano, flash, lite, small, 8b ...
    Light,
    /// nothing in the name says either way (and sonnet-class names): the safe middle
    Standard,
    /// the top of a family: opus, fable, pro, ultra, max, large ...
    Heavy,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Light => "light",
            Tier::Standard => "standard",
            Tier::Heavy => "heavy",
        }
    }
}

/// The parameter-count words of open models ("8b", "70b", "1.5b", "235b-a22b"): the size decides the tier.
fn billions(word: &str) -> Option<f64> {
    let digits = word.strip_suffix('b')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse::<f64>().ok()
}

/// Guess the tier from a model id or a route (`claude-api:claude-haiku-4-5`, `openrouter:z-ai/glm-5.2:free`). The
/// id is split into words at every character that is not a letter or a digit, so `pro` never matches inside
/// `provider` or `prompt`. A tier word wins over a size, and a heavy word wins over a light one when both appear.
pub fn capability_tier(model: &str) -> Tier {
    let lower = model.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    const HEAVY: &[&str] = &["opus", "fable", "pro", "ultra", "max", "large", "heavy", "reasoner"];
    const LIGHT: &[&str] = &["haiku", "mini", "nano", "flash", "lite", "small", "tiny", "instant", "air"];
    if words.iter().any(|w| HEAVY.contains(w)) {
        return Tier::Heavy;
    }
    if words.iter().any(|w| LIGHT.contains(w)) {
        return Tier::Light;
    }
    // Open models say their size: the largest number before a `b` decides.
    let biggest = words.iter().filter_map(|w| billions(w)).fold(None::<f64>, |acc, b| Some(acc.map_or(b, |a| a.max(b))));
    match biggest {
        Some(b) if b <= 14.0 => Tier::Light,
        Some(b) if b >= 100.0 => Tier::Heavy,
        _ => Tier::Standard,
    }
}

/// Sort `items` cheapest first. Priced items come first by price (ties by tier, then name); unpriced items come
/// after, by tier (light, standard, heavy) and then name. `price` is the reference cost in any one unit, `None` when
/// the route reports none (a free route should report `Some(0)`); `name` is the model id or route.
pub fn sort_cheapest_first<T>(items: &mut [T], price: impl Fn(&T) -> Option<u64>, name: impl Fn(&T) -> String) {
    items.sort_by_cached_key(|item| {
        let id = name(item);
        let tier = capability_tier(&id);
        match price(item) {
            Some(p) => (0u8, p, tier, id),
            None => (1u8, 0u64, tier, id),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_say_their_tier_and_unknown_names_sit_in_the_middle() {
        assert_eq!(capability_tier("claude-haiku-4-5-20251001"), Tier::Light);
        assert_eq!(capability_tier("gpt-5.4-mini"), Tier::Light);
        assert_eq!(capability_tier("gemini-2.5-flash-lite"), Tier::Light);
        assert_eq!(capability_tier("claude-opus-4-1"), Tier::Heavy);
        assert_eq!(capability_tier("claude-fable-5"), Tier::Heavy);
        assert_eq!(capability_tier("gpt-5-pro"), Tier::Heavy);
        assert_eq!(capability_tier("claude-sonnet-4-5"), Tier::Standard);
        assert_eq!(capability_tier("some-new-model-7"), Tier::Standard);
        // words, not substrings: "provider" and "prompt" are not "pro"
        assert_eq!(capability_tier("provider-prompt-model"), Tier::Standard);
        // open models by size
        assert_eq!(capability_tier("ollama:qwen3:8b"), Tier::Light);
        assert_eq!(capability_tier("openrouter:qwen3-235b-a22b:free"), Tier::Heavy);
        assert_eq!(capability_tier("ollama:qwen3-coder:30b"), Tier::Standard);
        // a route prefix never decides: the model words do
        assert_eq!(capability_tier("claude-api:claude-haiku-5-5"), Tier::Light);
    }

    #[test]
    fn unpriced_routes_sort_after_priced_ones_and_by_tier_not_by_name() {
        // Claude OAuth reports no price: alphabetically "fable" would head the list. It must come last.
        let mut routes: Vec<(&str, Option<u64>)> = vec![
            ("claude-fable-5", None),
            ("claude-haiku-4-5-20251001", None),
            ("claude-sonnet-4-5", None),
            ("gpt-5.4-mini", Some(80_000)),
            ("claude-opus-4-1", Some(2_000_000)),
        ];
        sort_cheapest_first(&mut routes, |r| r.1, |r| r.0.to_string());
        let order: Vec<&str> = routes.iter().map(|r| r.0).collect();
        assert_eq!(
            order,
            ["gpt-5.4-mini", "claude-opus-4-1", "claude-haiku-4-5-20251001", "claude-sonnet-4-5", "claude-fable-5"]
        );
    }

    #[test]
    fn equal_prices_break_by_tier_then_name_and_free_is_priced_zero() {
        let mut routes: Vec<(&str, Option<u64>)> =
            vec![("b-pro", Some(5)), ("a-pro", Some(5)), ("z-mini", Some(5)), ("free-one", Some(0))];
        sort_cheapest_first(&mut routes, |r| r.1, |r| r.0.to_string());
        let order: Vec<&str> = routes.iter().map(|r| r.0).collect();
        assert_eq!(order, ["free-one", "z-mini", "a-pro", "b-pro"]);
    }
}
