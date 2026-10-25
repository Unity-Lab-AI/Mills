//! What the orchestrating model is told while the mode is on: the directive (its job, the brief template, the
//! routing words) and the live roster briefing (which workers, what they are good at, their health). Rendered per
//! prompt build (the Prompt hook), so it belongs to the dynamic part of the prompt, never the cached prefix.
use crate::health::Health;
use crate::profile::{Profile, TaskKind, Worker};
use crate::tier::{capability_tier, sort_cheapest_first};

pub fn directive(profile_name: &str) -> String {
    format!(
"PGM orchestrator mode is ON (profile \"{profile_name}\"). You are the orchestrator: you plan, split the task into units a cheap worker can finish, write each worker an exact brief, dispatch it, and review what comes back before anything reaches the user. You do not do the workers' labour yourself.
Dispatch a unit with the swarm tool: spawn with model = \"auto\" (the orchestrator plugin picks the worker) or model = \"auto:<kind>\" where kind is one of research, summarize, implement, verify, debug, review, write, general. A concrete route passes through untouched. Or call pgm_dispatch {{ kind, brief }}: it picks the worker and answers with the exact swarm input to run; after a failure call it again with the same brief and it hands the unit to the next worker, up to the profile's attempts. If a spawn is refused as unroutable, split the brief or pick another kind; never do the unit on your own model to save time.
A brief has: Goal (one sentence) / Context (only what the worker needs: paths, snippets, constraints) / Exact steps / Constraints (what not to touch) / Freedom (what it may decide) / Done when (checkable) / Report back (what to return, in what shape). Keep briefs short: a worker's window is smaller than yours.
Review every result: wrong, partial or refused work goes back as a new brief (same kind, the plugin swaps to the next worker). Say in your final answer which workers did what."
    )
}

pub fn roster_briefing(profile: &Profile, health: &Health, now: u64) -> String {
    let mut out = format!("Roster ({}, mode {}):\n", profile.name, match &profile.mode { crate::profile::Mode::FreeOnly => "free only".to_string(), crate::profile::Mode::ProviderOnly(p) => format!("{p} only"), crate::profile::Mode::PaidOnly => "paid only".to_string(), crate::profile::Mode::Mixed => "mixed".to_string() });
    let admitted = profile.admitted();
    if admitted.is_empty() { out.push_str("  (no admitted workers: /pgm-orchestrator profile <name> or add workers)\n"); return out; }
    for w in admitted {
        let h = health.of(&w.route);
        let state = if h.benched_until > now { format!("benched {}s", h.benched_until - now) } else if h.in_flight > 0 { format!("{} in flight", h.in_flight) } else { "ready".to_string() };
        let kinds: Vec<&str> = w.good_at.iter().map(|k| k.as_str()).collect();
        let kinds_text = if kinds.is_empty() { "nothing listed".to_string() } else { kinds.join(" ") };
        let refused: Vec<&str> = h.refused_kinds.iter().map(|k| k.as_str()).collect();
        out.push_str(&format!("  {}  {}  window {}  brief budget {}  good at: {}{}  {}  ok {} / failed {}{}\n",
            w.route,
            if w.free { if w.route_says_free() { "free" } else { "marked free but the route is paid" } } else { "paid" },
            w.context_tokens.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
            profile.brief_budget(w).map(|b| b.to_string()).unwrap_or_else(|| "?".into()),
            kinds_text,
            if refused.is_empty() { String::new() } else { format!("  refuses: {}", refused.join(" ")) },
            state, h.successes, h.failures,
            if w.note.is_empty() { String::new() } else { format!("  ({})", w.note) }));
    }
    let kinds_without: Vec<&str> = TaskKind::ALL.iter().filter(|k| !profile.admitted().iter().any(|w| w.covers(**k) && !health.refuses(&w.route, **k) && !health.benched(&w.route, now))).map(|k| k.as_str()).collect();
    if !kinds_without.is_empty() { out.push_str(&format!("  No live worker right now for: {}\n", kinds_without.join(", "))); }
    out
}

/// The cost ladder (Jcode-Mods items 49 and 50): the admitted workers of the live profile, cheapest rung first, each
/// with the exact route to copy into a swarm spawn's `model`, its price when it has one, its tier guess when it has
/// none, what it is good at and how it is doing. Generated from the profile on every prompt build, never hand-written,
/// so it can neither name a worker the profile dropped nor a friendly id the provider would refuse.
pub fn cost_ladder(profile: &Profile, health: &Health, now: u64) -> String {
    let mut rungs: Vec<&Worker> = profile.admitted();
    if rungs.is_empty() {
        return String::new();
    }
    let price = |w: &&Worker| -> Option<u64> {
        if w.free {
            Some(0)
        } else {
            w.prompt_price_micros_per_m.map(|p| p + w.completion_price_micros_per_m.unwrap_or(0))
        }
    };
    sort_cheapest_first(&mut rungs, price, |w| w.route.clone());
    let money = |micros: u64| format!("${:.2}", micros as f64 / 1_000_000.0);
    let mut out = String::from("Cost ladder (cheapest rung first). Pass the route exactly as written as the spawn's model; a shortened or friendly id is refused, not guessed:\n");
    for (i, w) in rungs.iter().enumerate() {
        let cost = if w.free {
            "free".to_string()
        } else {
            match (w.prompt_price_micros_per_m, w.completion_price_micros_per_m) {
                (Some(p), Some(c)) => format!("{}/M in, {}/M out", money(p), money(c)),
                (Some(p), None) => format!("{}/M in", money(p)),
                _ => format!("price unknown, {} tier by name", capability_tier(&w.route).as_str()),
            }
        };
        let kinds: Vec<&str> = w.good_at.iter().map(|k| k.as_str()).collect();
        let h = health.of(&w.route);
        let state = if h.benched_until > now { format!("benched {}s", h.benched_until - now) } else { "ready".to_string() };
        out.push_str(&format!("  {}. {}  {}  good at: {}  {}\n", i + 1, w.route, cost, if kinds.is_empty() { "nothing listed".to_string() } else { kinds.join(" ") }, state));
    }
    out.push_str("Start a unit on the lowest rung that is good at its kind; climb a rung only after a failure or a refusal, and say why in the brief. Never put routine labour on the top rung or on your own model.\nReasoning effort is honoured only by models that expose it; on the others it is ignored with a warning, so do not expect an effort setting to make a cheap worker think harder: a harder unit goes to a higher rung.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::starter_profile;

    #[test]
    fn the_cost_ladder_comes_from_the_live_profile_with_exact_ids_and_the_effort_warning() {
        let mut p = starter_profile();
        p.mode = crate::profile::Mode::Mixed;
        p.workers.push(Worker::new("claude-api:claude-fable-5").with(200_000, &[TaskKind::Review], false));
        p.workers.push(Worker::new("claude-api:claude-haiku-5-5").with(200_000, &[TaskKind::Summarize], false));
        p.workers.push(Worker::new("openrouter:cheap/model").with(100_000, &[TaskKind::Implement], false).priced(100_000, 400_000));
        let ladder = cost_ladder(&p, &Health::new(), 10);
        // free workers first, then the priced one, then the unpriced by tier: haiku before fable
        let at = |s: &str| ladder.find(s).unwrap_or_else(|| panic!("{s} missing in {ladder}"));
        assert!(at("openrouter:cheap/model") < at("claude-api:claude-haiku-5-5"), "{ladder}");
        assert!(at("claude-api:claude-haiku-5-5") < at("claude-api:claude-fable-5"), "{ladder}");
        assert!(at("nvidia/nemotron-3-ultra-550b-a55b:free") < at("openrouter:cheap/model"), "{ladder}");
        assert!(ladder.contains("$0.10/M in, $0.40/M out") && ladder.contains("price unknown, light tier by name"), "{ladder}");
        assert!(ladder.contains("exactly as written") && ladder.contains("Reasoning effort is honoured only by models that expose it"), "{ladder}");
        // a worker the profile drops is not on the ladder
        p.workers.retain(|w| w.route != "claude-api:claude-fable-5");
        assert!(!cost_ladder(&p, &Health::new(), 10).contains("claude-fable-5"));
        // nothing admitted, nothing said
        p.workers.clear();
        assert_eq!(cost_ladder(&p, &Health::new(), 10), "");
    }

    #[test]
    fn the_directive_and_briefing_say_what_matters() {
        let d = directive("free");
        assert!(d.contains("profile \"free\"") && d.contains("model = \"auto\"") && d.contains("Done when") && d.contains("never do the unit on your own model"));
        assert!(d.contains("pgm_dispatch { kind, brief }") && d.contains("hands the unit to the next worker"));
        let mut h = Health::new();
        h.record("openrouter:z-ai/glm-5.2:free", TaskKind::Implement, crate::health::Outcome::Refused, 100, 2, 600, "no");
        let b = roster_briefing(&starter_profile(), &h, 200);
        assert!(b.contains("mode free only") && b.contains("nvidia/nemotron-3-ultra-550b-a55b:free  free  window 1000000"));
        assert!(b.contains("z-ai/glm-5.2:free") && b.contains("refuses: implement") && b.contains("benched 500s"));
        assert!(!b.contains("No live worker"), "{b}");
    }
}
