//! Context windows from every provider's catalog (DECISIONS A1 item 7). The host reads each provider's catalog (the
//! OpenRouter and OpenAI-compatible model lists, the local servers' serving windows, the static tables of the direct
//! providers); this module is the pure rule that applies them to a profile: a window is corrected down, never up,
//! never invented, and a media model (image, audio, video, speech, embeddings) never stays in a worker roster.
use crate::catalog::{correct_context_window, is_media_model};
use crate::profile::Profile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    /// tokens; None or 0 = the catalog does not say
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCatalog {
    pub provider: String,
    pub models: Vec<CatalogModel>,
}

impl ProviderCatalog {
    pub fn new(provider: &str, models: &[(&str, Option<u32>)]) -> ProviderCatalog {
        ProviderCatalog {
            provider: provider.into(),
            models: models
                .iter()
                .map(|(id, w)| CatalogModel {
                    id: (*id).into(),
                    context_window: *w,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowChange {
    pub route: String,
    pub before: Option<usize>,
    pub after: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowReport {
    /// windows filled in or lowered
    pub changed: Vec<WindowChange>,
    /// media models removed from the roster
    pub excluded_media: Vec<String>,
    /// workers whose window is still unknown after the catalogs (they are never selected)
    pub unknown: Vec<String>,
}

fn split_route(w: &crate::profile::Worker) -> (String, String) {
    let (head, model) = w.route.split_once(':').unwrap_or(("", &w.route));
    let provider = if w.provider.is_empty() { head } else { &w.provider };
    (provider.to_ascii_lowercase(), model.to_string())
}

/// The smallest window any catalog of this provider gives the model (None when none does).
pub fn catalog_window(catalogs: &[ProviderCatalog], provider: &str, model: &str) -> Option<u32> {
    catalogs
        .iter()
        .filter(|c| c.provider.eq_ignore_ascii_case(provider))
        .flat_map(|c| c.models.iter())
        .filter(|m| m.id.eq_ignore_ascii_case(model))
        .filter_map(|m| m.context_window.filter(|w| *w > 0))
        .min()
}

/// Apply the catalogs to the profile's workers (in place).
pub fn apply_catalog_windows(profile: &mut Profile, catalogs: &[ProviderCatalog]) -> WindowReport {
    let mut report = WindowReport::default();
    profile.workers.retain(|w| {
        let (_, model) = split_route(w);
        if is_media_model(&model) {
            report.excluded_media.push(w.route.clone());
            false
        } else {
            true
        }
    });
    for w in &mut profile.workers {
        let (provider, model) = split_route(w);
        let declared = w
            .context_tokens
            .map(|c| u32::try_from(c).unwrap_or(u32::MAX));
        let fixed = correct_context_window(declared, catalog_window(catalogs, &provider, &model));
        let after = fixed.map(|c| c as usize);
        if after != w.context_tokens {
            report.changed.push(WindowChange {
                route: w.route.clone(),
                before: w.context_tokens,
                after,
            });
            w.context_tokens = after;
        }
        if w.context_tokens.is_none() {
            report.unknown.push(w.route.clone());
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Mode, TaskKind, Worker};

    fn worker(route: &str, window: Option<usize>) -> Worker {
        let mut w = Worker::new(route).with(1, &[TaskKind::General], true);
        w.context_tokens = window;
        w
    }

    /// Mocked catalogs of four providers, none of them OpenRouter-only.
    fn catalogs() -> Vec<ProviderCatalog> {
        vec![
            ProviderCatalog::new("openrouter", &[("z-ai/glm-5.2:free", Some(200_000)), ("openai/text-embedding-3-small", Some(8_000))]),
            ProviderCatalog::new("claude-api", &[("claude-haiku-5-5", Some(200_000))]),
            ProviderCatalog::new("openai", &[("gpt-6.1-mini", Some(400_000)), ("whisper-1", None)]),
            ProviderCatalog::new("ollama", &[("qwen3:8b", Some(4_096)), ("llama3:8b", Some(0))]),
        ]
    }

    #[test]
    fn windows_come_from_each_providers_catalog_not_only_openrouter() {
        let mut p = Profile::new("m", Mode::Mixed, vec![
            worker("openrouter:z-ai/glm-5.2:free", None),
            worker("claude-api:claude-haiku-5-5", None),
            worker("openai:gpt-6.1-mini", None),
            worker("ollama:qwen3:8b", None),
        ]);
        let r = apply_catalog_windows(&mut p, &catalogs());
        let w: Vec<Option<usize>> = p.workers.iter().map(|w| w.context_tokens).collect();
        assert_eq!(w, [Some(200_000), Some(200_000), Some(400_000), Some(4_096)]);
        assert_eq!(r.changed.len(), 4);
        assert!(r.unknown.is_empty() && r.excluded_media.is_empty());
    }

    #[test]
    fn a_window_is_corrected_down_never_up() {
        let mut p = Profile::new("m", Mode::Mixed, vec![
            worker("openrouter:z-ai/glm-5.2:free", Some(1_000_000)), // declared too big: lowered
            worker("openai:gpt-6.1-mini", Some(32_000)),             // declared smaller than the catalog: kept
            worker("ollama:qwen3:8b", Some(4_096)),                  // equal: no change
        ]);
        let r = apply_catalog_windows(&mut p, &catalogs());
        let w: Vec<Option<usize>> = p.workers.iter().map(|w| w.context_tokens).collect();
        assert_eq!(w, [Some(200_000), Some(32_000), Some(4_096)]);
        assert_eq!(r.changed, [WindowChange { route: "openrouter:z-ai/glm-5.2:free".into(), before: Some(1_000_000), after: Some(200_000) }]);
    }

    #[test]
    fn nothing_is_invented_a_missing_or_zero_window_stays_unknown() {
        let mut p = Profile::new("m", Mode::Mixed, vec![
            worker("ollama:llama3:8b", None),          // catalog says 0
            worker("openrouter:not/listed:free", None), // not in any catalog
            worker("mystery:model", None),              // no catalog of that provider at all
        ]);
        let r = apply_catalog_windows(&mut p, &catalogs());
        assert!(p.workers.iter().all(|w| w.context_tokens.is_none()));
        assert_eq!(r.unknown.len(), 3);
        assert!(r.changed.is_empty());
        // a declared window survives a catalog that does not know the model
        let mut q = Profile::new("m", Mode::Mixed, vec![worker("openrouter:not/listed:free", Some(64_000))]);
        apply_catalog_windows(&mut q, &catalogs());
        assert_eq!(q.workers[0].context_tokens, Some(64_000));
    }

    #[test]
    fn media_models_leave_the_roster() {
        let mut p = Profile::new("m", Mode::Mixed, vec![
            worker("openrouter:openai/text-embedding-3-small", Some(8_000)),
            worker("openai:whisper-1", None),
            worker("openai:gpt-6.1-mini", None),
        ]);
        let r = apply_catalog_windows(&mut p, &catalogs());
        assert_eq!(r.excluded_media, ["openrouter:openai/text-embedding-3-small", "openai:whisper-1"]);
        assert_eq!(p.workers.len(), 1);
        assert_eq!(p.workers[0].route, "openai:gpt-6.1-mini");
    }

    #[test]
    fn several_catalogs_of_one_provider_give_the_smallest_window_and_ids_match_case_blind() {
        let cats = vec![
            ProviderCatalog::new("ollama", &[("Qwen3:8B", Some(32_000))]),
            ProviderCatalog::new("OLLAMA", &[("qwen3:8b", Some(8_000))]),
        ];
        assert_eq!(catalog_window(&cats, "ollama", "qwen3:8b"), Some(8_000));
        assert_eq!(catalog_window(&cats, "llmd", "qwen3:8b"), None);
    }
}
