# pgm-workflow-engine-internal (PGM_WorkflowEngine_Internal)

The Hyper Workflows engine (T-200). A workflow is a `pgm-workflow-internal` document; a **run** is one prompt sent
through it. The engine is plain code, no agent loop: it follows the graph, calls an AI only when a node says so, and
goes on until an `end`, a limit, a cancel or an unhandled failure. Spec: the team note `plans/hyper-workflows.md`
(sections 4, 5, 8.2, 14; the checks are R407 to R423). Depends on `pgm-workflow-internal`, `pgm-orchestrator-internal`,
tokio and futures-util; no network, no hub, no screen of its own.

| Module | What it gives you |
| --- | --- |
| `engine` | `Engine`: `start`, `resume` (from the run's log), `answer` (a question the run waits on), `replay` (no outside calls), `preflight`; `with_builtins()`; `implement` / `register` node kinds; `RunInput`, `RunOptions`, `RunLimits`, `RunReport` |
| `host` | `WorkflowHost` (AI, tools, HTTP, agents, people, emit, saved prompts, workflows for `sub-workflow`, projects, secrets, the log sink, snapshots, the clock: every method defaults to "not here"), `NodeKind` (one attempt of one kind of node), `NodeCall` (`lookup`, `render` for `{{templates}}`, `answer`), `NodeError` (`NodeError::ask`: the node needs a person), `NodeOutput`, `NoHost` |
| `kinds` | the built-in kinds (T-201): `ai` (`prompt`, `chat`, `summarise`, `jev`), `tools` (`tool` with custom HTTP calls, `agent`), `script` (the transform language), `orchestrate`; `model_chain` / `ask_model` (model choice and fallback, spec 5.4; the node's laws and their checks, spec 6) |
| `service` | the store and run API a hub serves under `/api/hyperwork/` (T-204): workflows per scope with versions, drafts and rights; laws, saved prompts and groups (T-202); runs with their logs, follow, cancel, answer, replay, resume after a restart; `/hyperwork` lines (`command`). No web server: `Service::handle(caller, method, path, query, body)`; the program around it says who is who (`Directory`) |
| `command` | the one `/hyperwork <name> [--project P] [--on runner] [--version V] <prompt>` parser for the page, HPM and jcode, and the nearest names for one that matches nothing (T-207) |
| `log` | `Prior::from_log`: what an earlier log of a run says (finished instances, open questions, answers, totals, active time) |
| `cancel` | `Cancel`: run, scope and node handles; cancelling one cancels what was made from it |
| `fake` | `FakeHost` (scripted AI answers per node or instance, call records, most-at-once, questions, a settable clock), `fake_engine()` with fake `prompt` / `tool` and the test kinds `test/inc`, `test/const`, `test/side-effect` |

## How a run goes

Every wire carries one **token** per instance of its scope: a value, *skip* (the way was not taken) or a *failure* (an
error nobody caught yet).
- A node fires when every wire into it has its token. With a value on each required input it runs; when only skips
  came it is **skipped** and skips its outputs (logged `skipped`); a failure passes straight through it.
- A failure is caught by a node's own `error` wire, by a `join` (`onFail`), by a loop (its `error` port) or by a
  sub-graph's output; an `end` that gets one, or a failure with no wire to go along, fails the run.
- The first `end` that gets a value ends the run (its name is the outcome); anything still running is cancelled.
- **Scopes:** the graph, a loop round, a parallel line, a sub-graph call. The engine's own kinds are `start`, `end`,
  `input`, `output`, `if`, `switch`, `parallel`, `join`, `loop`, `for-each`, `subgraph`, `approve`, `wait`, `set`,
  `emit`. Every other kind is a `NodeKind` given to the engine; a document using a kind this engine cannot run is
  refused before it starts (`engine.kind-missing`), as is one with validation errors.
- Each node instance has a **key**: its id, plus where it runs (`p@par:l2`, `inc@l:3`, `p@b:sg`, nested with `/`). The
  log is keyed by it, so a resumed run takes every finished instance from its last `node_end` and runs nothing twice.

Per node (inherited from its section, then the document's `defaults`): `timeoutSec`, `retry: {max, backoffSec,
factor, on: ["timeout","error","empty"]}`, `onError: fail | skip | continue-with-default`. Per run (`defaults.limits`,
with `RunOptions::limits` as the scope's ceiling; the lower wins): `maxSeconds` (active time, not waiting time),
`maxNodes` (500), `maxCost`, `maxParallel` (4, AI and other given kinds only).

**Waiting for a person** (`approve`, or a cut node that is not idempotent): the host's `ask_person` raises the question
and returns; the drive ends `waiting` with a `snapshot` (no worker held). `Engine::answer` goes on. A deadline is
checked whenever the run is driven: the program around the engine calls `Engine::resume` at the deadline.

## Decisions taken here (Gee to confirm; each is easy to change)

1. **Two crates** (open questions 1 and 12): the model (`pgm-workflow-internal`) needs no async runtime, so an editor or
   a viewer uses it alone; this crate runs graphs.
2. **A flow port carries the value it decided on.** `if`, `switch`, `approve` (and `jev`, T-201) put their input on the
   chosen port, so `approve.approved -> end.result` ends with the approved text, not with nothing.
3. **Loop body** = every node reachable from `again` and not from the loop's other ports. A node reachable both ways
   runs after the loop with the last round's values. A round that sends nothing back to `back` ends the loop by `done`
   (a "break").
4. **Lines** (`parallel` in `copies` / `split` mode) are the nodes between `out` and the join: config `join` names it,
   else the first `join` reached. A wire out of the lines other than into the join is refused. `fixed` mode is
   ordinary wiring: each named line gets the value.
5. **`join` first / any / n-of-m** cancels what only fed it once it is satisfied (running nodes through their cancel,
   waiting ones logged `skipped`, reason `cancelled`).
6. **The log is the snapshot's source.** `snapshot.json` (`WorkflowHost::save_snapshot`) is written when a run stops to
   wait; resume reads the log. `answer` events carry `by: user | default | timeout`.
7. **`wait`** sleeps inside the drive; long waits as timer wake-ups belong with triggers (T-186).

## The built-in kinds (T-201)

`Engine::with_builtins()` has them all; each reaches the world only through the host.

| Kind | What it does |
| --- | --- |
| `prompt` | one AI call: `system` / `user` templates (`{{inputs.x}}`, `{{node.n2.out.text}}`, `{{prompt:name}}`; a missing value is an error), `responseFormat: json` (a bad answer is asked again, `jsonRetries`, then `engine.bad-json`) |
| `chat` | the node's model and a `partner` (an AI critic) take turns until `maxTurns` or `stopWhen` (a condition on `value` / `turn`, or a phrase); outputs `messages`, `last`, `turns`, `stopped` |
| `summarise` | to a `target` (`bullets` / `chars` / `tokens`) in a `style`; long texts in pieces (`pieceChars`) then merged; a `keep` item that was in the text stays in the summary whatever the model did |
| `jev` | a decision: `pick` / `yes-no` / `score` / `rank` as fixed-shape JSON; `votes` (majority), `minConfidence` + `onLowConfidence` (`default`, `second-opinion`, `ask-person`, `error`); the chosen port carries the context |
| `tool` | a named tool (the host enforces the scope's allowlist: `engine.tool-denied`) or a custom HTTP call (`url` / `headers` / `body` templates, `{{secret:NAME}}` in headers only, `expect.status` and `expect.path`) |
| `script` | the transform language: `name = expression` lines, the `in` port as `input`; no loops, files, network or clock; `maxSteps` |
| `agent` | hands a task to a coding agent (`host.run_agent`) and waits for its final answer; not idempotent |
| `orchestrate` | planner → subtasks side by side → reviewer, `maxRounds`; workers by `workerModel` or by a pgm-orchestrator `workers` profile (`select` per subtask kind, fallbacks after) |
| `sub-workflow` | (the engine's own) another workflow by `name` / `range` from the host's store, as its own run linked by `parent`; depth at most 8 |

**Model choice** (spec 5.4): the node's `model`, else its sections' (innermost first), else the workflow's
`defaults.model`, else the host's default (the person's); an entry's `fallback` list is tried when a model is unreachable
or rate-limited (`model.unreachable` / `model.rate-limited` from the host), then `engine.model-unreachable`.

**A node may ask a person** (`NodeError::ask(questions, state)`): the attempt closes as `waiting`, the run waits, and
`Engine::answer` runs the node again with `NodeCall::answer` (and the `state` it gave in `NodeCall::asked`).

**The decision-model slot** (T-210, spec 5.7): a `jev` node without its own model uses the scope's decision model
(`WorkflowHost::decision_model`; on the hub the scope policy's `decisionModel`, e.g. JEV through OpenRouter with a local
Ollama model as its `fallback`), then the sections' and the workflow's.

## Laws and saved prompts (T-202, spec 6)

A node's AI calls follow the laws of four levels, in this order: the scope's standing laws (`WorkflowHost::law_book`,
`defaults`; on the hub the scope policy's `laws`), the workflow's `defaults.laws`, its sections' (outer to inner), the
node's own. Groups are expanded, each law comes once (the first place stays), `-law-x` at a lower level takes a law
away unless it says `lockable: false`. The laws go into the system part under a heading; `node_start` names their ids
and versions. A law's `check` (`regex`, `expr` on `value`, or `jev`: the decision model answers a yes / no question)
runs on every answer: `onFail` `retry` (asked again once, told which law), `error` (`engine.law-check`), `warn` (in
`node_end.warnings`). `{{prompt:name}}` is a saved prompt's latest version, `{{prompt:name@3}}` a pinned one. In the
service: `GET laws|prompts|groups`, `POST law|prompt|group` (every save a new version file), `POST law/delete` (a
group's members stay); export bundles carry what a workflow uses and import checks it.

Not here yet: custom-node sandboxes (T-203), large values as `values/<hash>` refs (T-204's later part), a
`sub-workflow` child that waits for a person (`engine.child-waits`).

## Use it in another project

```toml
[dependencies]
pgm-workflow-engine-internal = { path = "../VibeRoller/native/crates/pgm-workflow-engine-internal" }
```

```rust
use pgm_workflow_engine_internal::{Engine, RunInput, RunOptions, model::doc};
use std::sync::Arc;

let mut engine = Engine::new();
engine.implement("prompt", Arc::new(MyPromptKind));          // behaviour for a built-in kind
let wf = Arc::new(doc::parse(&std::fs::read_to_string("release-notes.json")?)?);
let problems = engine.preflight(&wf);                        // empty: it may run here
let report = engine.start(wf.clone(), RunInput::prompt("v5.24 notes"), Arc::new(MyHost), RunOptions::default()).await;
match report.status.as_str() {
    "ok" => println!("{:?}: {:?}", report.outcome, report.result),
    "waiting" => { /* store report.log; later engine.answer(wf, log, key, answers, "user", host, opts) */ }
    _ => eprintln!("{:?}", report.error),
}
```

## Tests

`cargo test -p pgm-workflow-engine-internal`. `tests/spec.rs` holds the engine's checks R407 to R417 (`r407_...` ...
`r417_...`) and more (sub-graphs, for-each, preflight, set / emit); `tests/kinds.rs` the built-in kinds' R418 to R423.
bridge/test/regress.js runs them once and maps the test names to the checks.
