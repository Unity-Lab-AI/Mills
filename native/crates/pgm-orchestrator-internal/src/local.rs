//! Local models as workers (DECISIONS A1 item 10). The models jcode's local provider (Ollama, llmd, LM Studio) serves
//! are roster members in every profile: free, with the window the server reports. The host lists them
//! ([`LocalModel`], one per backend and model); this module turns them into workers and merges them into a profile.
//! When several backends serve the same model there is ONE worker for it, and [`LocalRoster::backends_for`] lists the
//! backends so the host can pick one per session (sticky, `pgm-context-internal::route::Router`).
use crate::catalog::{is_local_provider, is_media_model};
use crate::profile::{Profile, TaskKind, Worker};
use crate::windows::{CatalogModel, ProviderCatalog};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One model one local backend serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModel {
    /// `ollama`, `llmd`, `lmstudio`, `local`
    pub backend: String,
    pub model: String,
    /// the serving window the server reports, in tokens (None = it does not say)
    pub context_window: Option<u32>,
}

/// The rank of an auto-listed local worker: after anything the user ranked themselves (default 100).
pub const LOCAL_RANK: u32 = 150;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalRoster {
    pub workers: Vec<Worker>,
    /// model -> the backends that serve it, first seen first
    pub backends: BTreeMap<String, Vec<String>>,
}

/// `provider:model` -> the model half.
fn model_of(route: &str) -> &str {
    route.split_once(':').map(|(_, m)| m).unwrap_or(route)
}

impl LocalRoster {
    /// The backends serving the model a route names (empty when it is not a listed local model).
    pub fn backends_for(&self, route: &str) -> Vec<String> {
        self.backends.get(model_of(route)).cloned().unwrap_or_default()
    }
    /// The same data as per-backend catalogs, for [`crate::windows::apply_catalog_windows`].
    pub fn catalogs(models: &[LocalModel]) -> Vec<ProviderCatalog> {
        let mut by: BTreeMap<String, Vec<CatalogModel>> = BTreeMap::new();
        for m in models.iter().filter(|m| is_local_provider(&m.backend.to_ascii_lowercase())) {
            by.entry(m.backend.to_ascii_lowercase()).or_default().push(CatalogModel {
                id: m.model.clone(),
                context_window: m.context_window,
            });
        }
        by.into_iter()
            .map(|(provider, models)| ProviderCatalog { provider, models })
            .collect()
    }
}

/// Workers for the listed local models. Not local, or media (embeddings, speech, …): skipped. One worker per model;
/// its window is the SMALLEST window among the backends that report one (never up), unknown when none does (such a
/// worker is listed but never selected). `good_at` is `general` only: nothing is claimed about quality.
pub fn local_roster(models: &[LocalModel]) -> LocalRoster {
    let mut out = LocalRoster::default();
    let mut windows: BTreeMap<String, Option<u32>> = BTreeMap::new();
    let mut order: Vec<String> = vec![];
    for m in models {
        let backend = m.backend.trim().to_ascii_lowercase();
        if !is_local_provider(&backend) || m.model.trim().is_empty() || is_media_model(&m.model) {
            continue;
        }
        let list = out.backends.entry(m.model.clone()).or_default();
        if !list.contains(&backend) {
            list.push(backend);
        }
        if !order.contains(&m.model) {
            order.push(m.model.clone());
        }
        let known = m.context_window.filter(|w| *w > 0);
        let slot = windows.entry(m.model.clone()).or_insert(None);
        *slot = match (*slot, known) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    for model in order {
        let backends = &out.backends[&model];
        let mut w = Worker::new(&format!("{}:{}", backends[0], model));
        w.context_tokens = windows[&model].map(|c| c as usize);
        w.good_at = vec![TaskKind::General];
        w.free = true;
        w.rank = LOCAL_RANK;
        w.note = format!("local model on {}", backends.join(", "));
        out.workers.push(w);
    }
    out
}

/// Add the local workers to a profile; a worker the profile already lists (same route, or the same model on a local
/// backend) is kept as configured. Returns the routes added. Whether a worker is *admitted* is still the profile mode's
/// call (`provider_only openrouter` lists but does not admit them).
pub fn merge_local(profile: &mut Profile, roster: &LocalRoster) -> Vec<String> {
    let mut added = vec![];
    for w in &roster.workers {
        let listed = profile.workers.iter().any(|p| {
            p.route.eq_ignore_ascii_case(&w.route)
                || (is_local_provider(&p.provider.to_ascii_lowercase())
                    && model_of(&p.route) == model_of(&w.route))
        });
        if !listed {
            profile.workers.push(w.clone());
            added.push(w.route.clone());
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Mode;
    use crate::select::select;
    use crate::health::Health;

    fn lm(b: &str, m: &str, w: Option<u32>) -> LocalModel {
        LocalModel { backend: b.into(), model: m.into(), context_window: w }
    }
    fn models() -> Vec<LocalModel> {
        vec![
            lm("ollama", "qwen3-coder:30b", Some(65_536)),
            lm("ollama", "nomic-embed-text", Some(2_048)),
            lm("llmd", "qwen3:8b", Some(32_768)),
            lm("ollama", "qwen3:8b", Some(4_096)),
            lm("ollama", "mystery:1b", None),
            lm("openrouter", "z-ai/glm-5.2:free", Some(200_000)),
        ]
    }

    #[test]
    fn the_roster_lists_local_models_with_their_window_free_and_without_media() {
        let r = local_roster(&models());
        let routes: Vec<&str> = r.workers.iter().map(|w| w.route.as_str()).collect();
        assert_eq!(routes, ["ollama:qwen3-coder:30b", "llmd:qwen3:8b", "ollama:mystery:1b"]);
        let coder = &r.workers[0];
        assert_eq!((coder.context_tokens, coder.free, coder.rank), (Some(65_536), true, LOCAL_RANK));
        assert_eq!(coder.good_at, [TaskKind::General]);
        assert!(coder.route_says_free());
        assert_eq!(r.workers[2].context_tokens, None, "an unreported window is not invented");
    }

    #[test]
    fn one_model_on_two_backends_is_one_worker_with_the_smaller_window() {
        let r = local_roster(&models());
        let q = r.workers.iter().find(|w| w.route.ends_with("qwen3:8b")).unwrap();
        assert_eq!(q.context_tokens, Some(4_096));
        assert_eq!(r.backends_for("llmd:qwen3:8b"), ["llmd", "ollama"]);
        assert_eq!(r.backends_for("ollama:qwen3:8b"), ["llmd", "ollama"], "any route of the model lists every backend");
        assert!(r.backends_for("ollama:nope").is_empty());
        assert!(q.note.contains("llmd, ollama"));
    }

    #[test]
    fn catalogs_per_backend_feed_the_window_correction() {
        let c = LocalRoster::catalogs(&models());
        let names: Vec<&str> = c.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(names, ["llmd", "ollama"], "only local backends");
    }

    #[test]
    fn every_profile_mode_lists_them_and_free_only_admits_them() {
        let roster = local_roster(&models());
        let configured = Worker::new("openrouter:z-ai/glm-5.2:free").with(200_000, &[TaskKind::General], true);
        for mode in [Mode::FreeOnly, Mode::Mixed, Mode::PaidOnly, Mode::ProviderOnly("openrouter".into()), Mode::ProviderOnly("ollama".into())] {
            let mut p = Profile::new("x", mode.clone(), vec![configured.clone()]);
            let added = merge_local(&mut p, &roster);
            assert_eq!(added.len(), 3, "{mode:?}: listed in every profile");
            assert!(p.workers.iter().any(|w| w.route == "ollama:qwen3-coder:30b"));
        }
        let mut free = Profile::new("f", Mode::FreeOnly, vec![configured.clone()]);
        merge_local(&mut free, &roster);
        let admitted: Vec<&str> = free.admitted().iter().map(|w| w.route.as_str()).collect();
        assert!(admitted.contains(&"ollama:qwen3-coder:30b") && admitted.contains(&"llmd:qwen3:8b"), "{admitted:?}");
        assert!(free.problems().iter().all(|p| !matches!(p, crate::profile::ProfileProblem::FreeFlagWithoutFreeRoute { .. })));
        // and a brief is routable to a local worker
        let s = select(&free, &Health::new(), TaskKind::General, 1_000, 0);
        assert!(s.routes.contains(&"ollama:qwen3-coder:30b".to_string()), "{:?}", s.skipped);
        assert!(s.skipped.iter().any(|(r, why)| r == "ollama:mystery:1b" && why.contains("unknown")));
    }

    #[test]
    fn a_configured_worker_wins_and_is_not_listed_twice() {
        let roster = local_roster(&models());
        let mine = Worker::new("ollama:qwen3-coder:30b").with(32_000, &[TaskKind::Implement, TaskKind::General], true).rank(5);
        let other_backend = Worker::new("llmd:qwen3-coder:30b").with(16_000, &[TaskKind::General], true);
        let mut p = Profile::new("x", Mode::Mixed, vec![mine.clone(), other_backend]);
        let added = merge_local(&mut p, &roster);
        assert_eq!(added, ["llmd:qwen3:8b", "ollama:mystery:1b"]);
        assert_eq!(p.workers[0], mine, "the user's rank, window and kinds are kept");
        // merging twice adds nothing
        assert!(merge_local(&mut p, &roster).is_empty());
    }
}
