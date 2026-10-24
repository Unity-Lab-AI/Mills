# pgm-context-internal (PGM_Context_Internal)

Fewer tokens and more cache hits for every model provider, without taking anything away from what the model is told.
Design: Jcode-Mods `TOKENS.md`. Pure Rust; no model inference in these layers.

| Module | What it gives you |
| --- | --- |
| `tokens` | `for_model(id) -> Box<dyn Counter>`: exact BPE counts for the OpenAI families (tiktoken `o200k_base` / `cl100k_base`), a calibratable estimator for Claude and local models; every counter says whether it is `exact()` |
| `ledger` | `parse_usage(&Value)` for the Anthropic / OpenAI / Gemini usage shapes → `Usage { input, output, cache_read, cache_write }`; `Ledger` with totals, per-provider split, hit ratio, JSONL persistence; `Prices` with Anthropic's cache rule |
| `assembly` | `Segment { id, class: Static | Session | Turn, text }` + `Caps::for_model(id)` → `plan()`: stable order, hashes, breakpoints after big-enough stable blocks, a `lint` naming volatile text ("3 min ago", timestamps, random ids, live counters) that sits in a stable segment and busts the cache |
| `shape` | `Shaper::shape(key, raw, turn)`: ANSI and trailing whitespace off, repeated lines folded, an unchanged re-read → one-line reference, long output capped with a stash handle; `fetch(handle, from, to)` brings lines back |
| `report` | `TurnRecord` = a `Turn` plus an optional `shaped: [ShapedOutput { tool, original_tokens, shaped_tokens, stashed }]` (old ledger lines parse with it empty); `SavingsReport::from_records` → per tool, per session, cache hit ratio, totals; `render()` for `/context report`, `to_json()` for the API |
| `retrieve` | the retrieval filter, OFF by default (`FilterConfig`, `Mode::{Off, Shadow, On}`): `score` / `select` keep the best earlier `Chunk`s within a token budget (pinned and the newest N turns always, a `pair_id` pair never split); `apply` in Shadow records an `AbRecord` without changing the request; `AbSummary` gives mean savings and the regret rate |

```rust
use pgm_context_internal::{assembly::{plan, Caps, Class, Segment}, tokens, ledger::Ledger, shape::{Shaper, Limits}};

let counter = tokens::for_model("claude-sonnet-5-5");
let p = plan(&[Segment::new("tools", Class::Static, tools_json), Segment::new("laws", Class::Static, laws),
               Segment::new("project", Class::Session, overlay), Segment::new("user", Class::Turn, message)],
             Caps::for_model("claude-sonnet-5-5"), &*counter);
for hit in &p.lint { eprintln!("move {} to the turn: {} ({})", hit.segment, hit.pattern, hit.snippet); }
let prompt = p.render();                       // most stable first, the same bytes every turn
let breakpoints = p.breakpoints();             // ids to put cache_control after (Anthropic)

let mut ledger = Ledger::new();
ledger.note_usage(now_ms, "anthropic", "claude-sonnet-5-5", "sponge", session_id, &response["usage"]);
println!("{}", ledger.summary_line());         // "12 turns · input 3,100 paid + 180,000 cached …"

let mut shaper = Shaper::new(Limits::default(), Some(&stash_dir));
let shaped = shaper.shape(Some("read:src/main.rs"), &tool_output, turn);
```

Depends on nothing else in pgm-libs. Rules: nothing is removed from what the model is told; every shaping pass is
lossless or reversible with a handle; the ledger measures before any layer is turned on by default.
