//! Provider catalogs (DECISIONS A1 items 7 and 8): context-window correction, media-model exclusion and the
//! free-only proof against a catalog the caller fetched (the fetch is I/O and lives in the plugin).
use crate::profile::{Mode, Profile};
use serde::{Deserialize, Serialize};

/// The smaller known window: correct down, never up, never invent (None only when both are unknown).
pub fn correct_context_window(declared: Option<u32>, catalog: Option<u32>) -> Option<u32> {
    match (declared, catalog) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Words (split on non-alphanumerics) that mark a model that is not a text worker.
const MEDIA_WORDS: &[&str] = &[
    "image",
    "images",
    "imagen",
    "audio",
    "video",
    "tts",
    "whisper",
    "embed",
    "embedding",
    "embeddings",
    "sora",
    "veo",
    "speech",
    "transcribe",
    "diffusion",
    "dalle",
];
/// Substrings that cross word boundaries.
const MEDIA_SUBSTRINGS: &[&str] = &["dall-e", "text-to-speech"];

/// Media models (image, audio, video, speech, embeddings) never belong in a worker roster.
pub fn is_media_model(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    MEDIA_SUBSTRINGS.iter().any(|s| l.contains(s))
        || l.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| MEDIA_WORDS.contains(&w))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub model_id: String,
    pub provider: String,
    /// USD per million tokens
    pub prompt_price_per_m: f64,
    pub completion_price_per_m: f64,
}

/// Providers whose models run on the user's own machine: free by definition.
pub fn is_local_provider(provider: &str) -> bool {
    matches!(provider, "ollama" | "llmd" | "lmstudio" | "local")
}

fn money(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0');
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    format!("${int}.{:0<2}/M", frac)
}

/// A `free_only` profile must be provably free: every admitted route is a local provider or sits in the catalog at
/// 0/0. Anything else is a named failure (`openrouter/x: completion $0.20/M`, `openrouter/y: not in the catalog`).
/// Other modes pass (nothing to prove).
pub fn prove_free(profile: &Profile, catalog: &[CatalogEntry]) -> Result<(), Vec<String>> {
    if profile.mode != Mode::FreeOnly {
        return Ok(());
    }
    let mut bad = vec![];
    for w in profile.admitted() {
        let provider = if w.provider.is_empty() {
            w.route.split(':').next().unwrap_or("")
        } else {
            w.provider.as_str()
        };
        if is_local_provider(provider) {
            continue;
        }
        let model = w.route.split_once(':').map(|(_, m)| m).unwrap_or(&w.route);
        let name = format!("{provider}/{model}");
        match catalog.iter().find(|c| {
            c.provider.eq_ignore_ascii_case(provider) && c.model_id.eq_ignore_ascii_case(model)
        }) {
            None => bad.push(format!("{name}: not in the catalog")),
            Some(c) => {
                let mut parts = vec![];
                if c.prompt_price_per_m > 0.0 {
                    parts.push(format!("prompt {}", money(c.prompt_price_per_m)));
                }
                if c.completion_price_per_m > 0.0 {
                    parts.push(format!("completion {}", money(c.completion_price_per_m)));
                }
                if !parts.is_empty() {
                    bad.push(format!("{name}: {}", parts.join(", ")));
                }
            }
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{TaskKind, Worker};

    fn entry(p: &str, m: &str, a: f64, b: f64) -> CatalogEntry {
        CatalogEntry {
            model_id: m.into(),
            provider: p.into(),
            prompt_price_per_m: a,
            completion_price_per_m: b,
        }
    }
    fn free_profile(routes: &[&str]) -> Profile {
        // the :free marker and the free flag make the worker admitted; the catalog is the proof
        Profile::new(
            "f",
            Mode::FreeOnly,
            routes
                .iter()
                .map(|r| Worker::new(r).with(100_000, &[TaskKind::General], true))
                .collect(),
        )
    }

    #[test]
    fn context_windows_correct_down_never_up_never_invent() {
        let cases: &[(Option<u32>, Option<u32>, Option<u32>)] = &[
            (Some(1_000_000), Some(256_000), Some(256_000)),
            (Some(8_000), Some(32_000), Some(8_000)),
            (Some(8_000), None, Some(8_000)),
            (None, Some(32_000), Some(32_000)),
            (None, None, None),
        ];
        for (d, c, want) in cases {
            assert_eq!(correct_context_window(*d, *c), *want, "{d:?} {c:?}");
        }
    }

    #[test]
    fn media_models_are_excluded_text_models_are_not() {
        for id in [
            "openai/dall-e-3",
            "openai/whisper-1",
            "openai/tts-1-hd",
            "openai/text-embedding-3-small",
            "google/imagen-4",
            "openai/sora-2",
            "google/veo-3",
            "openai/gpt-4o-audio-preview",
            "google/gemini-2.5-flash-image",
            "black-forest-labs/flux-video",
            "nomic/embed-text",
        ] {
            assert!(is_media_model(id), "{id} should be media");
        }
        for id in [
            "z-ai/glm-5.2:free",
            "anthropic/claude-haiku-5-5",
            "qwen/qwen3-coder:30b",
            "google/gemma-4-31b-it:free",
            "meta/imaginative-chat",
        ] {
            assert!(!is_media_model(id), "{id} should stay");
        }
    }

    #[test]
    fn a_catalog_of_zero_prices_proves_free() {
        let p = free_profile(&["openrouter:a/b:free", "openrouter:c/d:free"]);
        let cat = [
            entry("openrouter", "a/b:free", 0.0, 0.0),
            entry("openrouter", "c/d:free", 0.0, 0.0),
        ];
        assert_eq!(prove_free(&p, &cat), Ok(()));
    }

    #[test]
    fn a_priced_or_missing_route_is_a_named_failure() {
        let p = free_profile(&[
            "openrouter:x:free",
            "openrouter:y:free",
            "openrouter:z:free",
            "openrouter:ok:free",
        ]);
        let cat = [
            entry("openrouter", "x:free", 0.0, 0.2),
            entry("openrouter", "y:free", 0.05, 0.0),
            entry("openrouter", "ok:free", 0.0, 0.0),
        ];
        let err = prove_free(&p, &cat).unwrap_err();
        assert_eq!(
            err,
            [
                "openrouter/x:free: completion $0.20/M",
                "openrouter/y:free: prompt $0.05/M",
                "openrouter/z:free: not in the catalog"
            ]
        );
    }

    #[test]
    fn local_providers_are_free_by_definition() {
        let p = Profile::new(
            "l",
            Mode::FreeOnly,
            ["ollama:qwen3-coder:30b", "llmd:m", "local:x", "lmstudio:y"]
                .iter()
                .map(|r| Worker::new(r).with(64_000, &[TaskKind::General], true))
                .collect(),
        );
        assert_eq!(prove_free(&p, &[]), Ok(()));
    }

    #[test]
    fn the_proof_only_applies_to_free_only_profiles_and_ignores_excluded_workers() {
        let paid = Worker::new("openrouter:p/q").with(100_000, &[TaskKind::General], false);
        assert_eq!(
            prove_free(&Profile::new("m", Mode::Mixed, vec![paid.clone()]), &[]),
            Ok(())
        );
        // a paid worker listed in a free-only profile is excluded (admitted() drops it), so it is never used
        assert_eq!(
            prove_free(&Profile::new("f", Mode::FreeOnly, vec![paid]), &[]),
            Ok(())
        );
    }

    #[test]
    fn money_keeps_two_decimals_and_small_prices() {
        assert_eq!(money(0.2), "$0.20/M");
        assert_eq!(money(3.0), "$3.00/M");
        assert_eq!(money(0.0004), "$0.0004/M");
    }
}
