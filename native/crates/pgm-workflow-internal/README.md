# pgm-workflow-internal (PGM_Workflow_Internal)

Hyper Workflows as data: a workflow is boxes (nodes) with sockets (ports) joined by wires (edges). This library holds
everything about the graph that needs no screen, no hub and no network. Spec: the team note `plans/hyper-workflows.md`
(sections 3, 4.4, 6.1, 9.1, 11.1, 13; the checks are R401 to R406). It depends on `pgm-core-internal` only (plus serde).
The engine that runs a graph (T-200) and the node kinds' behaviour (T-201) are not here: they ask this library
"is this valid, what are its ports".

| Module | What it gives you |
| --- | --- |
| `model` | `Document`, `Node`, `Edge`, `Section`, `Subgraph`, `PortDecl`, `Defaults`: the section 13 JSON shape. Every struct keeps the fields it does not know (`extra`) and writes them back |
| `doc` | `parse` / `from_value` (older `schema` migrated on read, a **newer** one refused with a clear message, 1 MB cap), `write` (canonical pretty JSON), `canonical` |
| `types` | `PortType` (`text number bool json file messages error flow list<T>`), `flows_into` (same type, number to text, anything to json, lists by item), `Ports` / `PortSpec` |
| `kinds` | `KindManifest` (spec 11.1), the `KindRegistry` trait, `DefaultRegistry` (the built-in kinds' port signatures: start, end, input, output, note, subgraph, prompt, chat, summarise, jev, tool, script, agent, orchestrate, sub-workflow, approve, if, switch, parallel, join, for-each, loop, wait, set, emit) + `register` for custom kinds, `schema_check` (a small JSON-schema subset) |
| `validate` | `validate(&doc, &Options) -> Report` for every rule of 3.3: errors and warnings with a `code` and a message naming the node / port / edge; `ScopeInfo` answers "does this law / prompt exist, can this scope reach this model" |
| `diff` | `diff(&v1, &v2) -> DocDiff`: added / removed nodes, changed config keys, rewired edges, sub-graphs, `summary()` lines |
| `version` | `parse_ref("name@3")`, `Channels` (draft / published / tags, next number), `publish` (refused with errors, needs a message), `restore`, `kind_compatible` |
| `shapes` | `Law`, `Prompt`, `Group` (spec 6.1) with their checks, `Prompt::fill`, `expand_laws` |
| `expr` | `{{inputs.x}}` / `{{node.n2.out.summary}}` / `{{x\|default:"y"}}` / `{{prompt:name}}` templates and a safe condition language (compare, `contains`, `matches`, `in`, `and`/`or`/`not`, `len`, number maths): our own, no code runs |
| `store` | `load_dir` (lists `*.json`; a bad file is moved aside as `<file>.bad-<yyyymmdd-hhmmss>` and the listing goes on; a newer schema is left alone), `load_file`, `save` (atomic, canonical) |

## Validation codes

Errors: `start.missing` `start.multiple` `end.missing` `start.in-subgraph` `node.misplaced` `node.duplicate` `edge.duplicate`
`edge.endpoint` `edge.node` `edge.port` `edge.type` `input.multiple` `input.required` `graph.cycle` `loop.no-limit`
`loop.limit` `loop.condition` `retry.max` `wait.timeout` `approve.timeout` `onError.invalid` `condition.syntax`
`subgraph.ports` `subgraph.recursive` `subgraph.depth` `subgraph.missing` `kind.unknown` `kind.version` `config.invalid`
`law.missing` `prompt.missing` `section.node` `section.duplicate` `port.type` `port.duplicate` `template.syntax`
`template.node` `template.port` `graph.too-big`. Warnings: `node.unreachable` `model.unreachable` `subgraph.unused`
`template.input` `template.root`.

A cycle is allowed only through a `loop` node (the graph minus its loop nodes must have no cycle). A loop needs a
limit (`maxIterations`, 1 to 100; a `count` loop's `count` is its limit). Sub-graphs nest at most 8 deep
(`Options::max_depth`), never contain themselves, and their declared inputs / outputs must match their `input` /
`output` nodes by name and type. The ports of a `subgraph` node are those declared ports.

## Use it in another project

```toml
[dependencies]
pgm-workflow-internal = { path = "../VibeRoller/native/crates/pgm-workflow-internal" }
```

```rust
use pgm_workflow_internal::{doc, validate, diff, store, DefaultRegistry, Options};

// read (older schemas migrate, a newer one is an Err naming the problem), check, compare, write
let text = std::fs::read_to_string("release-notes.json")?;
let mut wf = doc::parse(&text)?;
let registry = DefaultRegistry::new();                  // built-in kinds; .register(manifest) adds custom ones
let report = validate(&wf, &Options::new(&registry));   // .scope(&my_scope) to check laws, prompts, models
for i in report.errors() { eprintln!("{} {}", i.code, i.message); }
if report.is_valid() { /* may be published: version::publish(..) */ }
wf.nodes.retain(|n| n.id != "n7");
println!("{:#?}", diff(&doc::parse(&text)?, &wf).summary());
store::save(std::path::Path::new("release-notes.json"), &wf)?;

// a folder of workflows: bad files are moved aside, the rest are loaded
let loaded = store::load_dir(std::path::Path::new("workflows"));
```

To teach it a kind the library does not know, implement `KindRegistry` (or register a `KindManifest` in
`DefaultRegistry`); to let validation check laws, saved prompts and models against a store, implement `ScopeInfo`
(every method defaults to "yes", so nothing is a false error without one). The library never touches the network, the
hub or a screen; its only side effects are the files `store` is asked to read, write or move aside, and a notice
pushed to `pgm_core_internal::notices` when it does.

## Provisional decisions (Gee to confirm)

These answer the spec's section 19 questions for Gee (1 to 3) so that T-199 could be built; each is easy to change.

1. **One crate, not several.** Model, validation, expressions, versions and the store are one library
   (`pgm-workflow-internal`); the modules are the seams if it is ever split (the likely first split is `expr`).
2. **Our own tiny expression language**, no dependency: `{{path}}` templates and a one-line condition language
   (`expr.rs`: about 400 lines, regex subset included). Nothing in it can run code, read a file or loop.
3. **Data ports only, with `flow` as a port type.** There is no separate "flow port" kind of wire: a flow port is a
   port of type `flow` ("go now", no value), which the spec needs for `jev` choices, `approve`, `if` and `switch`. A
   flow output may feed a `flow` input or a `json` input (a go signal with an empty value, "anything to json").

Other choices made on the way, also provisional:
- `loop` ports: in `in` (required) and `back` (many); out `again`, `done`, `limit`, `round`, `error`. The cycle goes
  `... -> loop.in`, `loop.again -> ...`, `... -> loop.back`. The spec's example edge `e6` is fixed this way in the fixture.
- `end.result` takes many edges (only one line of a graph reaches a given end), so several ways can lead to one `end`.
- `script` outputs come from `config.outputs`: a plain name is a `text` port, `{name, type}` is typed.
- A node's input default is `config.<port>`, `config.defaults.<port>` or `config.inputs.<port>` (spec 3.1 says only "in the
  node's config").
- Schema 0 (a document with no `schema`: `connections` instead of `edges`, node `type` instead of `kind`) is a
  stand-in for the first real migration, to prove the pattern; no such files exist.
- `kindVersion` is the kind's **major** version; a node made with a major that is not installed is `kind.version`.
- `diff` matches nodes and edges by id and ignores `pos` (moving a box is not a change) and the publish stamp.
- Canonical text: pretty JSON in the declared field order, unknown fields last, sub-graph keys sorted.

## Tests

`cargo test -p pgm-workflow-internal`. `tests/spec.rs` holds the checks R401 to R406 (`r401_...` ... `r406_...`) over
`tests/fixtures/release-notes.json` (spec section 13's example, canonical, with its loop fixed and unknown fields
added); bridge/test/regress.js runs them once and maps the test names to the checks.
