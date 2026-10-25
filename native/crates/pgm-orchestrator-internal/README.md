# pgm-orchestrator-internal (PGM_Orchestrator_Internal)

The policy half of `/pgm-orchestrator` (Jcode-Mods DECISIONS.md A1): the orchestrating model plans, briefs and
reviews; cheap workers from a **profile** do the labour. Pure Rust, no I/O; the plugin around it (Jcode PGM
`pgm/plugins/pgm-orchestrator`) does config, persistence and the hooks.

| Module | What |
| --- | --- |
| `profile` | `Worker { route, provider, context_tokens, good_at, rank, free, trust, note }`, `Mode::{FreeOnly, ProviderOnly(p), PaidOnly, Mixed}`, `Profile` with `admitted()`, `problems()` (single-homed kinds, unknown windows, free flags on paid routes) and `brief_budget()`; `starter_profile()` |
| `health` | `Health::record(route, kind, Outcome, now, strikes_before_bench, bench_secs, note)`: refusals and unavailable routes bench at once, errors take strikes, successes clear, cancellations are not strikes, a refusal is permanent for that kind; `classify_failure(text)` |
| `select` | `select(profile, health, kind, brief_tokens, now) -> Selection { routes (chosen + fallbacks), skipped (route, why) }`: specialists > free > load > rank > successes; `parse_ask("auto:debug")`, `infer_task_kind(brief)` |
| `prompt` | `directive(profile_name)`, `roster_briefing(profile, health, now)` and (5.19.0) `cost_ladder(profile, health, now)`: the admitted workers cheapest first with exact routes, prices or tier guesses and the effort warning, for the dynamic part of the prompt |
| `tier` | (5.19.0) `capability_tier(model) -> Tier::{Light, Standard, Heavy}` from the words of the id; `sort_cheapest_first(items, price, name)`: priced by price, unpriced after them by tier then name |
| `attempts` | `Unit { id, kind, brief }`, `Timeline` of `Attempt { unit_id, attempt, worker, outcome: AttemptOutcome::{Ok, Failed, Refused, TimedOut}, started_ms, ended_ms }` with `render(unit_id)` for `/pgm-orchestrator timeline`; `next_attempt(profile, health, timeline, unit, now_ms) -> NextStep::{Submit { worker, attempt }, Done { worker }, GiveUp { reason }}`: bounded by `Profile::max_attempts` (default 3), never a worker that already failed the unit, fallback order from `select` |
| `spend` | `SpendLedger::record(Usage, Option<&Worker>)`: tokens (input, output, cache read/write) and cost per worker and in total, cost from the worker's per-million prices (`prompt_price_micros_per_m`, …, USD micros) or 0 for free workers, never invented (shown as `unpriced`); `render()` for `status` |
| `catalog` | `prove_free(profile, &[CatalogEntry])` (every route of a free-only profile at 0/0 in the catalog, or a named failure; local providers are free), `correct_context_window(declared, catalog)` (the smaller known value), `is_media_model(id)` |
| `route_error` | `classify_route_error(status, body) -> RouteError::{Refusal, RateLimited, Transient, Fatal}` (one commented pattern table); `Health::record_route_error` (refusal benches on the first strike, 429 and 5xx are strikes, a context-length error is nobody's strike); `needs_context_at_least(body)` |

Rules kept from the fork (ADDEDFEATURES §1): load before rank, two candidates per kind, refusals permanent, our own
cancellations are not strikes, unroutable means fail (never the orchestrator's own model), never fill a worker's
window, the user picks workers, text-only output counts as "usable", trust never loosens safety.
