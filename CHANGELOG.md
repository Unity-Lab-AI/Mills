# VibeRoller changelog (formerly Mills)

Semantic versioning (hub law 14, with the versioning law in LAWS.md since 3.3.0): `versions.json` holds the project's
version and every module's. MAJOR = an entire refactor / rework or a change to 50 % or more of the codebase, MINOR =
moderate to large features or anything that might break, PATCH = fixes, minor features, small changes; a higher bump
resets the lower numbers to 0, and every merge into develop moves the version up. Each change bumps every module it
touches and the project by its largest bump, and adds a section here. Every change of any tracked file is
logged here in the same commit, newest first (hub law 15).

## 5.13.7 — 2026-10-25

PATCH: **Sponge's session and presence decisions in the specs and their checks** (T-282, T-258). Each Work session gets
its own worktree (work/<user>.<sid>, T-292); only working sessions count against the limits, and a run that stalls or
goes silent without saying done is ended and counts as idle; closing a session hides it; one menu (the dots and the
right-click) with delete (force-stops its runs), archive and the person's own folders; do not disturb only mutes
notifications; active time vs AI working time with a 15-minute gap (T-263). R197 checks per-session worktrees, open
while running, hide / unhide, folders and delete-while-running; R198 counts working sessions. Team notes
plans/multi-session.md and plans/presence.md. No behaviour changed.

## 5.13.6 — 2026-10-25

PATCH: **the specs for presence and multi-session Work, as checks first** (T-259, T-283). Team notes
`plans/presence.md` (presence per device, most active wins × working / accepting work, the derived label and its
order, appear away / offline, do not disturb, custom text with an until, the protocol on the existing room long poll)
and `plans/multi-session.md` (the session model, its storage next to work-current.json as a mirror, the six states
and which wins, the routes, limits 5 per host / 3 per provider with 429, what must keep working, compatibility).
`spec()` checks R196 (presence), R197 (sessions) and R198 (limits) in --rust-hub; they say SPEC until T-259 / T-284 /
T-285 build them. No behaviour changed.

## 5.13.5 — 2026-10-25

PATCH: **switching to the Rust hub alone is the config line and a graceful restart** (T-256). vr-run read
`rustHub.alone` only when it started, so on the live server the graceful restart after 5.13.3 kept the JavaScript hub
running behind the Rust one, and the swap needed a restart of the whole service (which also ends the Work run that asks
for it). vr-run now reads it again whenever a build asks for a restart (exit 75): the next build starts with `--alone`
and the JavaScript hub behind it stops (it stays the last resort); setting it back to false starts it behind again (both directions in R195).
R195 (written first, under the testing law: it failed on 5.13.4 with the JavaScript hub still behind).

## 5.13.4 — 2026-10-25

PATCH: **the testing law** (LAWS.md). Five rules for every project's tests: check what the code does, not how it does
it (a refactor that changes no behaviour must not break a check; static source checks only where there is nothing to
observe); name checks like sentences; fix or delete a flaky check the same day (no retry-until-green); coverage is a
floor (every change gets a check for what it does and how it fails); a bug gets a failing check before its fix (the
CHANGELOG names it; a feature not built yet gets a `spec()`).

## 5.13.3 — 2026-10-24

PATCH: **the Rust fallback chain actually fills up** (T-157). The auto-deploy marked a version's native build good only
when it verified the hub, but the native build of a version runs after that verify, so no build was ever on the good
list and every `use-build.sh` let `keep-builds.sh` delete all previous builds: after 5.13.2 went live only its own build
was left. New `native/mark-good.sh <dist> <commit>` puts a build first in `<dist>/good`; the auto-deploy calls it once a
build passed its trial, before putting it in service (both targets). R194 runs four deploys through mark-good + use-build
and checks the current build plus the two before it stay.

## 5.13.2 — 2026-10-24

PATCH: `bridge/work-current.json` (the default account's Work state, hub data like work.json) is gitignored; on the
server it showed as an untracked change and `worktree.js live` refused to go live ("main has uncommitted changes").

## 5.13.1 — 2026-10-24

PATCH: **a merge never pushes to "upstream"**. `worktree.js merge` pushed the integration branch to every remote but
origin, so merging Jcode-PGM tried to push our private develop to github.com/1jehuang/jcode (it failed only for want of
GitHub credentials). `pushAll` now skips `upstream` (the original `publish` keeps to pull updates from), and `publish`
makes that remote fetch-only (`no-push://fetch-only`); the existing Jcode-PGM repo was set the same way by hand. R193
(a real push into two bare remotes: the mirror gets it, upstream does not). bridge/worktree.js 2.3.1. Also: the host log redaction test (host/control.rs) builds its fake token instead of
spelling a 48-character hex string, which R31 (no hashes in tracked files) caught once the file was tracked.

## 5.13.0 — 2026-10-24

MINOR: **hosting controls on the hub page** (board T-160; gee: how can I look at what my AI host is doing now that the
hub changed?). The person who runs an AI host monitors, controls and configures it from Settings → My AI hosts, not only
locally through the `vr-host` TUI.
- **New host capability `control`** (announced in hello with the others; a host without it keeps working exactly as
  before and shows no buttons). The native host reports a small status (`POST /node/status`, on hello, every ~15 s and
  right after a control: version, start time, paused (and by whom), in flight, last job, last update line, the usage
  windows, the providers' login problems; never a message or a path) and answers `control` jobs: `pause`, `resume`
  (the off file; a host paused from the hub keeps listening so the hub can resume it, lends nothing, and the hub counts
  it offline for work), `restart` and `update` (the same request files as the local control app: they wait until nothing
  is in flight), `logs` (the last 50 lines of its log, redacted of the token, provider keys and key-like words).
- **Routes** (owner only, an admin is not the owner): `GET /api/hosts/status` (the person's own hosts: the latest status,
  online / since, windows, who is using it now: the hub knows its jobs in flight, listed as kind, and for a Work run the
  person and project, never content) and `POST /api/hosts/control { id, action }` (refused with "This host is older than
  5.13: update it to control it from here." for a host without the cap, 403 for anybody else's host, 400 for an unknown
  action).
- **Page** (web/index.html 1.18.0): per host a live line (online since, version and uptime, paused, in use by whom,
  the 5 h / 7 d windows, last update, login problems) refreshed every 10 s while the panel is open; Pause / Resume /
  Restart (two clicks) / Update now / Show log; an older host shows what the hub knows and a note instead of buttons.
- **Tests:** unit (job parsing, cap check, status cleaning, redaction, log tail); R191 (a native host in the scratch
  setup: status arrives, pause from the hub pauses it and the page list says so, resume, the log lacks the token, a host
  without the cap, a non-owner and an unknown action are refused, restart ends the host); R188 expects the new cap.

## 5.12.0 — 2026-10-24

MINOR: **the fallback chain: Rust alone, two Rust fallbacks, JavaScript only as the last resort** (board T-157 step one;
Sponge: "step one now [the server on the Rust hub alone], tested thoroughly via automated tests … we leave JavaScript as
a fallback … at least 1 if not 2 Rust fallbacks … we only use the non-updated JavaScript version if all else fails").
- **vr-run** now serves, in this order: the newest build (`rust`), the previous good build (`rust-fallback-1`), the one
  before (`rust-fallback-2`), and only when every Rust build fails (or none exists) the JavaScript hub alone
  (`javascript-fallback`). Until now `--alone` / `rustHub.alone` removed the JavaScript hub completely, so there was no
  last resort; it is kept again, started only as that last resort, loudly (log, chat, status), kept up while it serves
  (a crash restarts it after 5 s; its exit 75 tries the Rust builds again at once). The decision order is a pure,
  unit-tested function (`decide`, `serving_label`).
- **/api/status** (from this machine) shows `serving` in both hubs (vr-run hands it down as `VR_SERVING`).
- **bridge/server.js 4.2.2:** honours `MILLS_PORT` (vr-run's last resort named the port, the hub ignored it and took
  `config.port`).
- **Two good builds always kept:** native/keep-builds.sh (current + the 2 newest good, unchanged and now checked by
  R192); the auto-deploy also records `last-good-2` next to `last-good`.
- **The law** (LAWS.md "The JavaScript hub stays as the last fallback"): frozen, not removed; new features are Rust-only,
  but the JavaScript hub must keep booting and serving the basics and keep reading every shared data file; the run checks
  the chain before every go-live and never removes the JavaScript hub without Sponge's word.
- **Suite:** R192 (the chain names itself in 4 situations incl. `--alone`; keep-builds keeps two good builds; the real
  JavaScript hub started alone boots and serves status, the page, the sign-in gate and sign-in, the chat room, the
  board, one Work run with the fake Claude and the AI host link: `bridge/test/js-fallback.js`, also runnable alone).
- Config the go-live sets and how to roll back: team note deploy/full-rust-swap.md.

## 5.11.3 — 2026-10-24

PATCH: **the proving ground** (board T-183; Sponge: "drive a fake version that talks to the hub … separately
isolatable from the main running systems … a spongetest or a geetest account … a separate database … a random project
that gets small changes done to it as we run tests"). `node bridge/test/proving.js start|stop|status|reset|login|work|
hosts` (`bridge/test/PROVING.md`): a persistent sandbox hub from the worktree's own code on port 7795 (7796 behind
with `--rust-hub`), its data under /srv/apps/sharedwork/_proving (outside every repo), accounts spongetest (admin) and
geetest, the chip-away project `Sponge/proving-ground` (private on Forgejo), real Claude through the driving Work run's
own relay (refuses to start without it), test AI hosts on demand for routing scenarios. Never touches the live hub, its
data, accounts or hosts. First results (T-183): a real Work turn as spongetest edited, tested, committed and pushed a
branch to the chip-away repo; three real jcode sessions decided the retrieval filter and replay (Jcode-Mods TOKENS.md).
R1 now syntax-checks only the `.js` files in bridge/test (PROVING.md lives there).

## 5.11.2 — 2026-10-24

PATCH: **a new law in LAWS.md: finish the whole job, test it in throw-away hubs, go live once.** Changes accumulate on
the work branches until every part of a job is done; tries happen only in the suite's scratch hubs; one merge and one
go-live at the end. And a Work run never ends while background agents it started are still working: a run is one
Claude process per message, and its agents die with it (that, not the live hub, cut agents off all day on
2026-10-24). No code changed.

## 5.11.1 — 2026-10-24

PATCH: **notification sounds with a pattern of their own** (board T-177). Sponge: "rather than simply being a ding, I
would like more of a pattern of some kind, with distinct sound patterns."
- **Patterns** (the new default, `web/index.html` `PATTERNS`): each kind is a motif told apart by rhythm and timbre as
  much as by pitch: a message is a soft two-note chime (falling), an @mention / private message a quick rising run, a
  reply to you a call-and-answer phrase, an incoming call a ringing trill played twice, a board reminder a tick-tock
  with a closing note. Every note has a soft attack and decay, a quiet overtone an octave up and a low-pass, so it
  sounds like a small bell rather than a beep; all under two seconds. Still Web Audio only, no sound files.
- **Classic** (the short beeps of before) stays as a choice: Settings → Sound style. The test buttons play the style
  picked in the menu right now, so the two can be compared before saving.
- **Two new volumes**: Incoming call and Board reminder (they followed the @mention volume before and still start from
  it). Calls ring with the `call` pattern (`web/calls.js`), board reminders tick with `board` (`web/board.js`).
- The hub keeps the new keys: `sound.set` (only `classic` / `patterns`) and `sound.call` / `sound.board` (0–100), in
  both hubs (`bridge/server.js`, `vr-hub` `modules/profile.rs`, and `pgm-hcom-internal` `profile.rs`, mirrored to
  pgm-libs). An older page ignores them; this page on an older hub falls back to the @mention volume.
- R190: a distinct pattern per kind, Classic kept, the style and the volumes stored and clamped, the call and board
  call sites.

## 5.11.0 — 2026-10-24

MINOR: **sharing with scopes** (board T-176 part 9d; Jcode-Mods ARCHITECTURE.md §4). MINOR rather than a patch because it
adds a stored field, a route and enforcement in three places (Work, the API / products, Unity's use) and a Settings
editor; a separate version from 5.10.0 (9b / 9c), which is a different, already complete increment. Sharing a provider
used to be all or nothing; the default is unchanged (no entry = every model, every use), so nothing that works today
changes.
- **Scopes** (`ai_hosts.rs`: `Scope`, `scope_of`, `check_scope`, `AiHosts::set_scope`, `scope_check`). Per shared person
  and per `@group` the owner picks which models of that provider they may use and what for: `work` (Work runs), `api`
  (the /v1 API and the gateway products behind it) and `unity` (Unity's brain). Stored on the hub only, in the host
  record next to `share`: `scopes[providerId][user | "@group"] = { models: "*" | [..], uses: "*" | [..] }` in
  `ai-hosts.json`; the host never sees it and keeps announcing `allowedUsers`, so an older host works as before. All
  models ticked / all three uses stored as `"*"` (a model the host adds later comes along); both `"*"` removes the entry.
  The person's own entry wins over their group's; the owner always has everything.
- **Enforcement where the hub picks a provider.** `providers_for` and `provider` list only the allowed models for that
  person (`scope` on the view says what is limited; a provider with no allowed model left is not listed). A Work run
  (`work.rs` `run`) and every /v1 call (`openai.rs` `v1_as`, so products too) call `scope_check` and refuse with a message
  naming the owner and what they allow ("t-admin shares R189 Claude with you, but not the model x (they allow y)" / "...
  not for Work runs (they allow it for the API and products)"); /v1 answers 403 `permission_error`. A /v1 call that names
  a model the person may not use is refused rather than quietly answered by another model.
- **Settings → My AI hosts**: under each provider a "What each person may use" editor, per person and `@group`: the
  provider's models as checkboxes and Work / API / Unity as toggles, saved at once through the new owner-only
  `POST /api/hosts/scope { id, provider, user, models, uses }` (validated: models must be the provider's own, uses work /
  api / unity, not the owner; `/api/hosts` carries `providers[].scopes`).
- Unit tests (default all, a model outside refused, a use outside refused, the owner unaffected via `scope_locked`, the
  group's scope for its members, the person's own scope wins, unknown uses dropped, the narrowed view); R189
  (`--rust-hub`, a registered host with two providers: t-user limited to one model and the API sees only that model in
  `/api/check`, is refused another model on /v1 and a Work run without `work`, then allowed once it is switched on; the
  group scope and the own-wins rule; the stored shape; the host's config untouched).
- Not done: Unity's brain does not pick a shared provider yet (it uses the hub's own `claude` and the hosts' engines), so
  `unity` is stored, validated and checked by `scope_check(.., "unity", ..)` but has no call site until it does.
  JavaScript hub: no mirror (the Rust hub answers `/api/hosts/scope`).

## 5.10.0 — 2026-10-24

MINOR: **VibeRoller as the gateway** (board T-176; Jcode-Mods WORKPLAN 9b and 9c). MINOR rather than a patch or a
second part of 5.9.0 because it adds a hello capability, a new provider kind on the hub and a second event schema the
page reads (all additive, but new protocol surface); 5.9.0 is a different feature (the token ledger).
- **9b. HubEvent is the schema the page consumes** (`harness.rs` `HubEvent::into_wire`, `wire`, `HUB_SCHEMA`;
  `work.rs` `on_event`; `web/index.html` `hubView`, `handleEvent`). Every harness event the hub passes on (its own Claude
  Code or Jcode PGM run, an AI host's Work run) now leaves as the Claude-shaped alias it always was plus ONE added object
  `hub` `{ v:1, kind: init | system | assistant | tool_results | result, harness, session, … }` (assistant: `blocks` of
  `text` / `tool_use`, `usage`; tool_results: `results` of `{ id, content, error }`; result: `error`, `text`,
  `durationMs`). The page's Work renderer reads `hubView(ev)`: the `hub` object first, else the same view derived from the
  alias (an older hub or host sends no `hub`), and no longer reads the alias fields itself. Nothing a person sees
  changes; an older page ignores `hub` and renders the alias. Unit test (both harnesses give the same structure for the
  same run, alias untouched); R187 (static + the page's own `hubView` on sample events in both modes; with `--rust-hub`
  a Claude run and a Jcode PGM run of the same shape give the same HubEvent structure).
- **9c. Jcode PGM on the AI host, opt-in** (native host only). A provider in `ai-node.json` of kind `jcode` (or
  `"harness": "jcode"`) with `jcodeCommand`, `jcodeProvider`, `jcodeArgs`, plus `"allowWork": true`: the host announces the
  new hello capability **`work-jcode`** (`pgm-net-internal` `CAP_WORK_JCODE`, `caps_for`) and offers the provider; the hub
  lists a `jcode` provider of a host only when that cap was announced (`clean_providers`), shows it as kind `host-jcode`
  ("AI host · Jcode PGM"), refuses to build a job for one without the cap (`run_work`), and sends the Work job (the legacy
  host payload plus the optional `harness` and `stateModel`) only to the owner (Work runs on the host's own computer);
  `/v1` and Unity answers through it are refused (it runs Work). On the host `jcode_spec` (host/work.rs) starts
  `jcodepgm run --ndjson --no-update [-p] [-m] [--resume] <message>` in the person's worktree with a private jcode home per
  person at `<host folder>/jcode/<person>/jcode-home` (the team MCP servers as `mcp.json`, the system prompt as the prompt
  overlay, no auto-update / telemetry / auto-poke: the hub's own jcode harness code), maps its events with the same
  `JcodeHarness`, and sessions resume (`claudeSessionWhere` = the host). **Default off:** a host without such a provider,
  or without `allowWork`, says hello with exactly `relay update update-bin` as before and its Claude path is unchanged
  (Gee's host is untouched). An older hub drops the unknown kind; an older host never announces the cap. `/api/check`
  `aiNodes[].caps` (optional) shows what each host announced. The control app lists `provider.<id>.jcodeCommand` /
  `jcodeProvider`. Unit tests (the host's command, home, per-person homes; the hub's cap filter; the cap list); R188
  (`--rust-hub`, the native host with the fake jcodepgm: a host with the cap runs a Work turn through jcode on the host,
  resumes it, a failure ends the run, mcp.json and overlay in its private home; a host without allowWork is never listed
  or pickable and says the old caps).
- Not done: 9d (per-provider scopes for sharing) is left for the next increment; sharing a jcode host to others is
  refused for now (only the owner may run Work on a host's computer).

## 5.9.0 — 2026-10-24

MINOR: **the token library is in the hub** (board T-167; Jcode-Mods TOKENS.md §4). The hub now shows the cache hit
ratio for API / product answers too, not only for Work, and the Work system prompt is checked by the library's lint so
a volatile part can never creep back into the cached prefix.
- **Unity's answers in the same ledger** (`unity_brain.rs`): the local brain's answer notes Ollama's `prompt_eval_count`
  / `eval_count` (final chunk of each round, summed over the tool rounds, normalised by `ollama_usage`; no cache numbers,
  Ollama does not report its KV prefix cache) under kind `ollama`, and the Claude brain (`claude -p`) the `result`
  event's Anthropic usage through the library's `parse_usage` (cache read / written kept) under `claude-cli`; one note
  per answer, never per chunk, shown in `/api/usage` `answers.by`. R186 (--rust-hub). (`parse_usage` could learn the
  Ollama shape in pgm-libs.)
- **`pgm-context-internal` 5.15.1 mirrored** (was 5.15.0; `shape.rs` gained `ShaperState` / `SeenEntry` / `state()` /
  `restore()` / `with_handle_prefix`) into `native/crates/` (LAWS.md library law: pgm-libs holds the original, the
  consumer copies it; `src/` identical, tree-sitter feature optional and off). Workspace member, listed in `NAMES.md`
  with the mirrored version; `pgm-ai-internal` depends on it.
- **Answers outside Work through the ledger** (`openai.rs` `complete`): every provider kind (openai, host-openai,
  anthropic, host-claude, host-codex, the Claude Code login / cli) now keeps the provider's own usage object next to the
  OpenAI-style numbers callers get (those are unchanged), carries Anthropic's `cache_read_input_tokens` /
  `cache_creation_input_tokens` and OpenAI's `prompt_tokens_details.cached_tokens` (Codex's Responses
  `input_tokens_details`) through `pgm_context_internal::ledger` into one shape (input paid, output, cache read, cache
  written), and adds it per person and provider kind to usage.json (`answers`, `Usage::note_answer`).
- **`/api/usage`** gets an optional key `answers: { turns, input, output, cached, written, hit, by: { <kind>: … } }`
  (hit = cached / (input + cached + written)); present once the person has had an answer, every existing key as it was
  (older pages ignore it).
- **The Work system prompt through the lint** (`work.rs` `system_segments`, `render_prompt`, `lint_segments`): the prompt
  is built as segments (the person's lines Session, the laws Static) and joined to the same bytes as before (R181 holds);
  a unit test builds it for hub and project worktrees and asserts the library's lint finds nothing, and the first prompt
  a hub builds is linted once with any hit named (segment, pattern) in the hub log.
- Tests: ledger normalisation for each provider shape, the answers sum and its persistence, the prompt lint; R185 (a /v1
  answer through the fake OpenAI-compatible endpoint with `cached_tokens` shows in `/api/usage` `answers`).

## 5.8.0 — 2026-10-24

MINOR: **`vr-host` is one place for everything.** Sponge, on a Linux PC: a Downloads copy and the real host in
`~/.local/share/viberoller/ai-host` ran side by side, `vr-host` was not on the PATH, settings meant typing on/off, and the
Codex login and Ollama were invisible. Now: run `vr-host` and set up, configure, check, update and start at boot from
inside it; install puts it on the PATH and updates keep the setup complete by themselves.
- **On the PATH, Windows and Linux** (new `pgm-platform-internal::path`): `ensure_on_path` / `ensure_on_path_in` /
  `path_status` / `on_path`. Linux: a link `~/.local/bin/vr-host` to the installed program (a stale link is replaced, a
  file that is not ours never) and, when `~/.local/bin` is not on the PATH, ONE block marked
  `# added by VibeRoller AI Host (vr-host)` in `~/.profile` (and `~/.bash_profile` / `~/.zprofile` when they exist), never
  twice. Windows: the program's folder added to the USER Path (PowerShell `SetEnvironmentVariable`, case and trailing
  backslash insensitive; the machine Path is never touched). `vr-host path` does it by hand.
- **Setup follows updates** (`vr-host` install.rs `ensure_setup`): `vr-host host` from an installed folder completes the
  setup at every start (PATH, VERSION.txt, the previous version's file removed once no update is on trial, a boot entry
  that runs another copy is named); hosts update in place and restart through it, so a step added there reaches every
  machine at its next update. `install` also puts vr-host on the PATH and, with no boot mode set and a config present,
  sets the default (Linux user service, Windows sign-in) and starts the host. `VR_HOST_NO_BOOT=1` / `VR_HOST_NO_PATH=1`
  keep a run off systemd / the PATH (tests). Hidden `vr-host setup` runs the same by hand.
- **`vr-host doctor [--json]`**: config valid and private, which copy is this (download or installed, both versions), the
  boot entry points at the installed program, vr-host on the PATH, the hub (from the host's status file), each login
  (Claude / Codex, with the fix text), Ollama (asked at OLLAMA_HOST, models counted; the host's status file now carries
  `detected: { ollama, models }`), updates (running vs hub version). `control::fix` repairs lockdown, path, boot, install.
- **The terminal app** (`tui.rs`): a first-run setup wizard (hub, id, masked token, or paste an ai-node.json / its path);
  a dashboard with a download-copy banner, an Updates row (running / hub / automatic / last) and a Providers row with a
  login mark per provider and the Ollama found; keys for start, stop, restart, pause, update now (shows the result), providers,
  settings, doctor, boot, install, log, help. Providers: switches, edit label / models / allowed users, add an
  OpenAI-compatible endpoint, remove (twice), adopt the found Ollama. Settings: switches flip with Space, text opens a
  prefilled editor (the token never shown), a line explains the setting. Doctor: Enter repairs a FIX row. Log: End = newest.
  The terminal is handed back around sudo / administrator prompts.
- A download without an ai-node.json acts on an installed host that has one. README.txt and the page describe the above.
- R184 (Linux): install from a scratch download into a scratch HOME, the PATH link and the profile block once, doctor,
  path, setup.

## 5.7.2 — 2026-10-23

PATCH (hotfix): **the native AI host's update and install behave on a machine with two copies.** Found on Sponge's
Linux PC: the user service ran the installed host in `~/.local/share/viberoller/ai-host`, a second copy sat in the
Downloads folder, and the automatic update to 5.7.1 never came.
- **A control command reaches the host that runs** (`vr-host` main): `status`, `update`, `stop`, `log`, `set` … run
  from a copy that is not running (a download) act on the installed host that is (found the way `install` finds it) and
  say so. Before, `./vr-host update` in the Downloads folder asked a copy that was not running, and nothing happened.
- **A second copy waits instead of looping** (`vr-host run`): when a host already runs in the folder, the supervisor
  waits for it to end and takes over then, saying so once. Before, it ended, and systemd (`Restart=always`) started it
  again every 5 s with "already running" in the log each time.
- **An update in steady use** (`host/runner.rs`): an automatic update that has waited 30 minutes for 3 quiet minutes
  takes the next moment with nothing in flight. A host its owner works through all evening never got 3 quiet minutes,
  so it never updated. LAWS.md says so next to the 3-minute rule.
- **`vr-host install` updates the install that runs** first (then the first one found, then the standard folder).
- R183 (Linux): the waiting supervisor, the control commands from a download, the 30-minute cap.

## 5.7.1 — 2026-10-23

PATCH (hotfix): **the AI host picks up a local Ollama, says why a Codex login is missing, and the page shows ChatGPT /
Codex and local models.** Sponge: only Claude showed up anywhere; Ollama (already running, so `ollama serve` said the
port was taken) and the Codex login were not picked up.
- **Ollama found by itself** (native host, `host/runner.rs`): at every hello and once a minute the host asks
  `http://127.0.0.1:11434/api/tags` (or the address `OLLAMA_HOST` names; a listen address `0.0.0.0` is reached at
  127.0.0.1). When it answers, the host lends it as the engine `ollama` (Unity) if none is configured, and offers a
  private provider `ollama` (an OpenAI-compatible endpoint at `/v1`, the models Ollama lists) if no endpoint for it is
  configured. A change (Ollama started later, a model pulled) is told to the hub with a fresh hello when nothing is in
  flight. Off with `"autoDetect": { "ollama": false }`; what the person configured is never replaced.
- **Login check at hello** (`host/relay.rs` `login_problem`): a Claude or Codex login the host cannot read is named in
  its log with the path it looked at. For Codex that includes the keyring case: Codex can keep its login in the
  system keyring, which the host cannot read; `cli_auth_credentials_store = "file"` in `~/.codex/config.toml` and a
  new `codex login` fix it (or `codexHome` on the provider).
- **The page** (`web/index.html`): the Work model menu and Settings → AI source list ChatGPT / Codex and local models
  too, greyed out and marked "not for Work": Work needs a Claude (or Jcode PGM) harness. They are picked under
  Settings → API access, where the provider names now read "AI host · ChatGPT / Codex", "AI host · local model",
  "AI host · Claude" instead of "undefined".
- The JavaScript host (`bridge/ai-node.js`) is unchanged; it still lends Ollama only when `engines` / `providers` name it.
- R182 (native host): Ollama found and offered with no configuration, private to its owner, answering the API; a
  missing Codex login named in the log; the page's greyed-out entries.

## 5.7.0 — 2026-10-22

MINOR: **Jcode PGM as a Work harness** (the first step of Jcode-Mods `EXTENSIBILITY.md` §3 and `ARCHITECTURE.md`:
a second coding harness next to Claude Code, picked per provider).
- **A `jcode` provider** (Settings → AI providers, admins only): a `jcodepgm` binary on the hub (a path, or the name
  on PATH), optional extra args, an optional jcode home, the jcode provider profile to run with (`-p`), and the models
  it may be asked for (`-m`; empty = jcode's own default). `Work::harness_of` (pgm-ai) says which harness a provider
  runs: "claude" (host, host-claude, anthropic, claude-cli), "jcode", or none (an endpoint / ChatGPT login: API and
  Unity only). `/api/model` accepts either harness; the old "not Claude" refusal names both.
- **A run on it:** `jcodepgm run --ndjson --no-update [-p …] [-m …] [--resume …] <message>` in the person's worktree,
  with a **private jcode home per person** (`<profile>/jcode-home`; the provider may name one): the hub's three MCP
  servers (pc, board, team) written as its `mcp.json`, the hub's system prompt (laws included) as its
  `prompt-overlay.md`, `JCODE_RUNTIME_DIR` inside it, no auto-update, no auto-poke, no telemetry, the person's own
  Claude Code MCP config kept out (`JCODE_DISABLE_CLAUDE_MCP`). Forgejo and `fj` env as for Claude Code.
- **One event shape for the page:** jcode's NDJSON (`start`, `text_delta`, `tool_start / tool_input / tool_exec /
  tool_done`, `tokens`, `done`, `error`) is mapped onto the shape the page renders already (`system/init`,
  `assistant` text and tool_use blocks, `user` tool_result, `result` with `is_error`), every mapped event tagged
  `harness: "jcode"`; other jcode events arrive as `{ type: "jcode", event }`. Usage, context and shared Work sessions
  work unchanged.
- **Sessions follow their harness:** `sessionHarness` in work.json; a Jcode PGM session id is never handed to Claude
  Code (`--resume`) and vice versa; switching providers starts fresh.
- R180 (`--rust-hub` / `--rust-alone`; the JavaScript hub does not learn this): a fake `jcodepgm` (test/fake-jcode.js)
  proves the add / refuse rules, the command line, the home (MCP answers through it, the overlay is there), the event
  mapping, resume, a failed run, and that going back to Claude never resumes a jcode session.
- pgm-libs 5.7.0 carries the same `pgm-ai` (library law).
- **A cache-stable Work system prompt** (Jcode-Mods `TOKENS.md`): the latest worktree freshness check ("N min ago")
  used to sit inside `--append-system-prompt`, so every run invalidated the provider's prompt cache for the whole
  system block. The worktree law stays in the prompt (the same bytes every run); the check result now travels at the
  end of each message (`fresh_note`). R181 proves the prompt carries no relative time or timestamp (the fake Claude
  reports `sysvolatile` / `syslaw`).
- **The session's prompt-cache ledger** (TOKENS.md): every `assistant` event's usage is summed per session (paid
  input, cache reads, cache writes, turns); `/api/usage` returns it as `context.cache` with the hit ratio, and the
  context meter shows "cache NN%" with the numbers in its title. R-usage checks the sums.
- **A proper `Harness` trait** (pgm-ai `harness.rs`, Jcode-Mods WORKPLAN 9a): `kind`, `command(&RunContext) ->
  CommandSpec` (program, args, env, cwd, stdin), `map_line(&str) -> Vec<HubEvent>`, `session_resume_args`, `banner`,
  `ended`; `ClaudeHarness` (stream-json passes through unchanged) and `JcodeHarness` (wraps the existing `Mapper`);
  one `HubEvent` enum (init, system, assistant, user, result, unknown, raw) in the page's shape. `Work::run` no longer
  branches on the harness: it builds a `RunContext`, asks the harness for the command and feeds each stdout line
  through `map_line`. A refactor only: no behaviour change, the page receives the same events; 6 unit tests.

## 5.6.1 — 2026-10-22

PATCH: **the library law** (LAWS.md). Sponge: code that two projects need is written once, in a library (pgm-libs and
what grows next to it: the Jcode PGM plugin SDK and host, the harness interface), and used from there; look for an
existing crate before writing, extend it rather than copy; copying a function between projects is the signal to move it
into the library. A library change follows the product release checklist (bump, lock, `--locked` build, tag, bump the
users). No code change.

## 5.6.0 — 2026-10-22

MINOR: **the full-Rust hub, step 6 (FR6): the Rust hub stands alone.** With the honest meter (R179 probes every
`/api/` path with the method the JavaScript hub expects) only `/api/logout` and `/api/admin/set-admin` were still
answered by the JavaScript hub; both are the Rust hub's now (`modules/mod.rs`, `modules/admin.rs`).
- **Alone mode:** `vr-hub --alone` (no `--back`): no JavaScript hub behind it; what it has no module for is 404 at
  once, never waited for. Config `"rustHub": { "alone": true }`: `run.js` then never starts `server.js` and starts a
  failing Rust hub again instead of handing the port to JavaScript; `vr-run --alone` (or that config) does the same
  and falls back to the previous good Rust build instead of the JavaScript hub. `/__vr-front` says `alone`.
- **The suite's third mode:** `node bridge/test/regress.js --rust-alone` runs every check against the Rust hub alone
  (R166 expects no JavaScript hub). The normal and `--rust-hub` modes stay until the JavaScript hub is deleted.
- **vr-run, the first swap's last net:** while `bridge/server.js` exists and every kept Rust build failed, the
  JavaScript hub takes the port alone (as run.js did), said in the chat; the newest build is tried again later.
  R177 proves it. With `alone`, or once server.js is gone, there is no such net: the previous good build is the net.
- The server stays on `run.js` + the JavaScript hub behind until the swap is made by hand (see plans/full-rust-hub.md
  § the swap): the swap is one config line (`rustHub.alone`), one service line (`vr-run`) and one restart at a quiet
  moment. bridge/run.js 1.6.0.

## 5.5.0 — 2026-10-22

MINOR: **the full-Rust hub, steps 2–5 (FR2–FR5 of plans/full-rust-hub.md): kept builds, a trial before the swap, a
Rust supervisor, bad data moved aside.** The JavaScript hub still runs behind the Rust one (FR6 retires it).
- **FR3 kept builds** (`native/keep-builds.sh`, `native/use-build.sh`; `build.sh` stages each build under
  `native/dist/<target>/<commit>/` and puts it in service through use-build.sh, which copies its files to
  `dist/<target>/` itself — the layout run.js, the Downloads and the hosts' updates read never changed — writes
  `current` and keeps the current build + the 2 newest good ones). The auto-deploy records a verified version's
  build in `dist/<target>/good` and, on a rollback, puts the last good build back in service with the checkout
  (before this a rollback kept running the new binary).
- **FR5 trial before the swap** (`deploy/trial.sh <vr-hub> <checkout>`): the staged build runs on a side port
  against a private copy of the settings, data and page; it must answer `/api/status` with the checkout's version,
  `/` 200 and `/api/version` 401 within 30 s. The auto-deploy stages (`VR_STAGE_ONLY=1`), tries, then puts the build
  in service and asks for the restart; a failed trial keeps the old build and says so in the team chat
  (`bridge/.announce/`). A build that cannot start never reaches anyone.
- **FR4 vr-run** (`native/crates/vr-run`, Windows + Linux): the supervisor in Rust. Runs the hub from the kept
  builds (`current`), the JavaScript hub behind it while `bridge/server.js` exists; health-checks `/api/status`
  every 10 s (3 misses = hung or dead → killed); a build that crashes 3 times within a minute or fails its health
  checks gives way to the previous good build (`bridge/.fell-back`, posted to the chat), the newest is tried again
  after 10 minutes, then hourly; exit 75 = restart on new code; Ctrl-C / SIGTERM stop it. `bridge/start.sh` runs it
  when it is built; the server's service still runs run.js until it is switched by hand (one line in
  deploy/viberoller-hub.service), so nothing changes there by itself.
- **FR2 bad data** (pgm-libs 5.6.0): a data file that is there but is not JSON is moved aside as
  `<file>.bad-<yyyymmdd-hhmmss>` (kept, never overwritten by the next save), the hub starts with defaults for that
  part and says so in the team chat (pgm-core `notices`). The chat's notice loop also posts `.fell-back` and
  `.announce/*.txt` (every 10 s).
- Suite: R175, R176, R177, R178 pass and are plain checks now; R179 (FR6) probes every `/api/` path the JavaScript
  hub names and lists the ones it still answers.

## 5.4.1 — 2026-10-22

PATCH: `bridge/unity.json` (the Rust hub's copy of Unity's state, data) is gitignored. It was untracked in the live
checkout, which made `worktree.js live` refuse ("main has uncommitted changes") until it was excluded by hand.

## 5.4.0 — 2026-10-22

MINOR: **the full-Rust hub, step 1 (FR1): a bad request never takes the Rust hub down.** Spec: team note
plans/full-rust-hub.md (board T-151 … T-157).
- `native/Cargo.toml`: the release build unwinds on panic instead of aborting. A panic inside one request answers that
  request 500 (`That request failed inside the hub; it was logged.`) and is logged with its method and path
  (`front.rs guarded`); the hub, its other connections and its pid go on. Before this any panic killed the whole hub
  and only the JavaScript hub taking the port kept the site up.
- Every std Mutex lock in the crates (690, both trees) recovers from a poisoning: `lock().unwrap()` →
  `unwrap_or_else(|e| e.into_inner())`. A lock held during a panic keeps working afterwards.
- Background loops (the Forgejo poll, the files sweep, the AI-hosts push, the notes watch) are started again after a
  panic, with a log line, never silently gone (`modules::forever`).
- Every `/api/` answer says who answered: `x-vr-by: rust` or `node` (FR6's progress meter).
- Test hook `GET /__vr-test/panic[?lock=gateway]`, only with `VR_TEST_HOOKS=1` in the environment (the regression
  runner's); 404 otherwise.
- Suite: `spec()` checks, which print `SPEC … not built yet` and never block a release (`--spec-strict` makes them
  fail): R174 FR1 (passes now), R175 FR2, R176 FR3, R177 FR4, R178 FR5, R179 FR6 (each names today's gap).
  The summary line counts them as "spec pending".

## 5.3.1 — 2026-10-22

PATCH: **"VibeRoller 4.1.0 is ready" on 5.x** (Sponge). The page compares the build every answer names (X-VR-Version)
with the one `/api/check` named. Since the Rust hub answers `/api/check` itself, those were two programs' builds,
each read when that program started: the Rust hub's, and the JavaScript hub's behind it on every answer it passes
on. When the two started apart (one restarted alone after a crash, or an older JavaScript hub kept running behind
a newer Rust hub), every page offered an update to the other build, reloaded onto it, and stayed put with that build
in its address. Now the Rust hub puts its own build on every `/api/` answer it passes on, so a page only ever sees
one; a JavaScript hub naming another build is logged once (a restart brings the two together). The page also drops
`?v=` from its address once loaded (it only busts the cache while reloading). Suite: R173. web/index.html 1.14.3.

## 5.3.0 — 2026-10-22

MINOR: Forgejo, fully, from a Work run: **`fj` (the Forgejo CLI) signed in as you**, and Forgejo's API through
`worktree.js`.
- `bridge/fj.js` (new) + `pgm-ai-internal` `fj.rs`: fj keeps its logins in one keys file per operating-system account,
  so on a shared machine everyone's runs would be one login. Every run now gets the person's own: a folder of theirs
  with that file (their token from Settings → My Forgejo, owner-only) and an `fj` wrapper first on the PATH that points
  fj at it (XDG_DATA_HOME / APPDATA for the fj process alone). The hub does it for runs here (under their profile),
  a host for the runs it takes (`bridge/.fj/<account>`, gitignored); the JavaScript and Rust hub / host all the same.
  No fj on the machine or no token: nothing is made, as before. Hosts that have not updated: fj unauthenticated, as
  before (nothing else changes; the token already travelled in the run's environment).
- `worktree.js`: `repos [words]` (the Forgejo repos you can see, private ones too), `repo-create <owner/name>`
  (`--private`, `--description`), `publish <owner/name> --project <name>` (the repo made when missing, a GitHub
  origin kept as `upstream`, origin = Forgejo, every branch + tag pushed, a `develop` made), `tag <name> [message]`
  (a tag on Forgejo at the project's integration branch, fetched here). As you, through your token: a run's allowed
  command, so Claude never needs an approval for it.
- `deploy/setup-server.sh` installs the pre-built forgejo-cli binary under tools/bin (building it needs OpenSSL
  headers, root-only); the run's system prompt and LAWS.md say what a run has.
- Suite: R172 (the wrapper, the keys file, owner-only, nobody else's, gone on disconnect; the static checks).
  bridge/fj.js 1.0.0, bridge/server.js 4.2.0, bridge/ai-node.js 3.4.0, bridge/worktree.js 2.3.0.

## 5.2.3 — 2026-10-22

PATCH: Admin → accounts has **Make admin / Remove admin** (`POST /api/admin/set-admin {username, admin}`), so the
founders (Gee, Sponge, Mills) are all admins without editing files. Nobody can remove their own admin, so there is
always one left. A profile's flag goes into its `config.json`. The default account's flag goes into
`config.local.json` (its permissions kept), and the Rust hub now reads it there too (pgm-identity `admin_in` /
`accounts`, mirrored to pgm-libs). `bridge/common.js` no longer lets the default account's `admin` reach the profiles.
Suite: R171. bridge/common.js 1.2.1, bridge/server.js 4.1.2, web/index.html 1.14.2.

## 5.2.2 — 2026-10-22

PATCH: `native/build-products.sh` also builds the products' Linux desktop apps (a tar.gz each) when the WebKitGTK
sysroot is there (`native/tauri-env.sh setup`, user space, no root; it is there on the server now, so VibeRoller's own
Linux desktop app is built and offered by the Downloads too), and attaches both the Windows and the Linux app to the
product's release on Forgejo.

## 5.2.1 — 2026-10-22

PATCH: the libraries' copy here is brought level with pgm-libs 5.4.0 (the products' needs; nothing the hub uses
changed): pgm-hcom-internal gets `profile` (profiles, prefs, pictures), the everyone-server's name from the config
(`homeServerName`, default "VibeRoller"), a "Tasks" permission category (VIEW_TASKS, MANAGE_TASKS; not given to
@everyone) and `Guilds::task_access`; pgm-collab-internal gets `Board::ensure_board` / `board_of`. The hub's own
behaviour is unchanged: the same servers, roles, boards and routes.
- The menu links to the products served here (`web/index.html` 1.14.1: "Also here: PGM Prod Suit · ShaDow Chat ·
  Hyper Prod Man"), on the hub's own page only.
- The desktop app takes its name from the build (`VR_APP_NAME`: the window title, the taskbar count, the tray), so
  the same app is each product's (native/crates/vr-desktop). `native/build-products.sh` builds the products' Windows
  apps (the product's address, name and identity built in) and attaches them to the product's release on Forgejo;
  `native/zip.js` zips them (this server has no zip program).

## 5.2.0 — 2026-10-22

**MINOR: PGM Gateway serves its products on this site** (board T-147 / T-150). A product registered with its port is
reachable at `/p/<id>/` (PGM Prod Suit: `/p/prodsuit/`) through the hub, so it needs no server set-up of its own
(no DNS, certificate or nginx change) and no change on any host. A subdomain of its own can come later; a product's
page works under both.
- The hub passes `/p/<id>/…` to the product on 127.0.0.1:port (pgm-net-internal `Backend::plain`, `proxy_to`):
  streamed, with the caller's address as nginx saw it, `X-Forwarded-Prefix`, and never anything a caller sent as
  `x-vr-*`, `x-pgm-*` or a forwarded address. `/p/<id>` redirects to `/p/<id>/`; a product's `/internal/` and `/__…`
  are never reachable this way; an unknown product, one without a port, or one that is down answers 404 / 503 for
  that path only. Nothing of the hub's own routes moved.
- Admin → PGM Gateway products takes the port and shows where each product is served (`web/index.html` 1.14.0).
- **The page is also the products' page** (`web/index.html` 1.14.0, `web/app.js` 1.1.2; the pgm-libs web kit): a
  product's server fills `__MILLS_PRODUCT__` (its id, name, base path, screens) and the same page shows that product:
  its name, only its screens on the menu, no Downloads / Connections, and everything it keeps in the browser (session,
  chat keys, settings, device id) under the product's own name, so on the shared site it never touches VibeRoller's.
  The hub fills nothing in and the page is VibeRoller's as before.
- Regression R170 (with `--rust-hub`) also runs a stand-in product behind `/p/`: the redirect, GET / POST with the
  path and body as sent, the forwarded headers, forged headers dropped, `/internal/` and `/__front/` closed, unknown /
  portless / down products.

## 5.1.1 — 2026-10-22

PATCH: what VibeRoller needs for the first PGM product (PGM Prod Suit, its own repository UnityAILab/pgm-prodsuit;
board T-147, plan: team note plans/pgm-products.md).
- **Admin → PGM Gateway products** (`web/index.html` 1.13.4): add a product (id, name, AI on or off), give it a new
  secret, remove it. It shows how many people each one linked and the file its secret is in; never the secret. The
  box shows only when the Rust hub is in front (the gateway is its own).
- **The board is shared with the products** (`web/board.js` 1.0.2, also the pgm-libs web kit 5.1.1): a page may set
  `window.BOARD_OPTS = { ai: false, product }` before loading it. Then there are no Claude buttons, the product's name
  shows in reminders, and it works on a page without Work. VibeRoller sets nothing and nothing changes here.
- The libraries' own repository: UnityAILab/pgm-libs (tags v5.1.0, v5.1.1), noted in native/crates/NAMES.md.
- Regression R170 also checks the products box and the board's options [static].

## 5.1.0 — 2026-10-22

**MINOR: the hub is also PGM Gateway** (board T-145, plan: team note plans/pgm-products.md). This is the first step
towards separate products (PGM Prod Suit, ShaDow Chat, Hyper Prod Man), each its own program with its own data and
accounts. The gateway gives them what only it has. Nothing changes for VibeRoller, its pages or any AI host: the host
protocol, its address and every route stay the same.

- **Products:** an admin registers one (`/api/admin/gateway/products`: list, add or renew its secret, remove). The
  secret is kept only in owner-only files (`bridge/gateway/products.json`, and `bridge/gateway/<id>.secret` for the
  product's program on this machine); no answer ever shows it.
- **The product link** (pgm-net-internal `product`): every request a product makes is signed with its secret (method,
  path, time, a one-time nonce, the person, the body). The gateway takes it only from this machine, never through a
  proxy, at most a minute old, never twice. `/internal/` stays closed from outside (nginx).
- **Sign in with your VibeRoller login:** `identity/verify` checks a VibeRoller username and password for the product
  (pgm-identity-internal `verify_password`: no session, no device lock, the same sign-in limits) and links that person
  to that product. A product acts only for people it linked.
- **AI for linked people:** `ai/providers` and `/internal/gateway/v1/*` (the same OpenAI- / Anthropic-compatible API
  as `/v1`, pgm-ai-internal `v1_as`) run as that VibeRoller account, so every host share applies with nothing changed
  on hosts. Only for products registered with AI on.
- The gateway's data is gitignored, in the deploy snapshot and in the data pack (`bridge/handoff.js` 1.2.4,
  deploy/pack-data, deploy/autodeploy.sh).
- Regression R170 (with `--rust-hub`): registering, signing, refusals (unsigned, replayed, moved, proxied, too old,
  wrong secret, not linked, not admin, renewed, removed), the sign-in limits, AI as the linked person.

## 5.0.0 — 2026-10-22

**MAJOR: the Rust code is rearranged into reusable libraries** (board T-136 … T-144, plan: team note
plans/pgm-libraries.md). Nothing behaves differently: no route, field, header, host-protocol message or data file
changed, and every regression check passes as before (normal and `--rust-hub`). The libraries know nothing about
VibeRoller's hub, so any other project can use them. Names: native/crates/NAMES.md.

### The libraries
- **pgm-core-internal** (PGM_Core_Internal, PsyGraphMediaCoreInternal): errors, base64, time (ISO exactly as
  JavaScript), SHA-256 / HMAC / AES-GCM / HKDF / ids, JSON files written atomically (and privately), file protection
  (Linux mode 600, Windows ACL), text helpers. The hub's five copies of the JSON / time helpers are now this one.
- **pgm-platform-internal** (PGM_Platform_Internal): starting programs the same way on Windows and Linux, standard
  folders, the Windows service and systemd units, start at boot, elevation.
- **pgm-net-internal** (PGM_Net_Internal): the HTTP server kit the hub's front runs on (requests, answers, CORS,
  hardening, the reverse proxy to the JavaScript hub), HTTP clients, long polls, the encrypted host link.
- **pgm-identity-internal** (PGM_Identity_Internal): accounts, passwords, sessions, device locks, sign-in limits,
  admins. The hub keeps only its own sign-in answer.
- **pgm-collab-internal** (PGM_Collab_Internal): the board, notes, projects / worktrees, Forgejo.
- **pgm-dist-internal** (PGM_Dist_Internal): signed releases, self-update, zips, version stamps.
- **pgm-hcom-internal** (PGM_HCom_Internal, PsyGraphMediaHumanCommunicationInternal): the chat and its end-to-end
  encryption, servers / channels / roles / moderation, calls, attachments, the Files library. What it needs from the
  hub is one trait (`ChatHost`).
- **pgm-ai-internal** (PGM_AI_Internal, PsyGraphMediaAIInternal): AI hosts, providers and the `/v1` APIs, Work runs,
  usage, Unity, the AI host program's job loop and the desktop's local API proxy. What it needs from the hub is one
  trait (`AiHub`).

### The programs
- vr-hub, vr-host and vr-desktop are thin around the libraries; vr-core only re-exports, so every older path works.
- The host link, its messages and its caps are unchanged: hosts that have not updated keep connecting and answering,
  and an updated host works with an older hub.
- How to use a library in another project: each crate's README.md and native/crates/NAMES.md.

## 4.1.4 — 2026-10-22

### Fixed (what Sponge ran into on the portal)
- **A Work reply survives a reload.** A reply's progress only came over the answer to the message that started it.
  After a reload the page could not see the reply at all until it was over. Opening a past session said "Claude is
  working, stop it first". Worse, the page sent the next queued message at once; the hub refused it because a reply
  was still going, and that message was lost.
  - Now the Rust hub keeps what a running reply has sent (`GET /api/work/attach`). The page saves, with the
    conversation, how far into the reply it has shown it.
  - After a reload, opening Work shows the reply live from exactly there, to its end, and only then sends what is
    queued. A PC tool request goes out again only while it still waits for an answer, so it never runs twice.
  - A reply whose stream breaks off (the network, a proxy) is picked up the same way.
  - A reply that just ended is still there for 5 minutes, so a reload right at the end sees how it ended.
  - The message when opening a past session during a reply now says what is going on.
- **No reload in front of you.** A new version is offered in a small bar ("VibeRoller X is ready. Reload now /
  Later"). The page takes it by itself only when its tab is in the background, and still never during a reply, a call
  or unsaved work.
- **Never a black page.** When opening the page fails, or takes over 20 s, and no screen is showing, the sign-in form
  comes back and says what went wrong (the session is kept). Before, an error after the sign-in form was hidden could
  leave just the background (seen after a hard refresh).
- **The GitHub Pages copy is retired:** opened there, the page goes to the hub (portal.unityailab.com).

### Tests
- R169 (joining a reply after a reload, `--rust-hub`; static checks for the update bar, the black-page guard and the
  Pages redirect).

## 4.1.3 — 2026-10-22

### Fixed: a page that reloads itself over and over
- **The page's self-update can no longer loop.** A signed-in page reloads onto a new build when a hub answer names
  another build than the one it loaded (X-VR-Version). After 4.1.1 went live, a Chrome tab kept reloading. Now the tab
  remembers the build it just reloaded for. If a hub answer still names that build within 10 minutes of the reload,
  and the page itself has another one, the page stays where it is: it says so once in the chat (both builds and the
  request) and writes the same to the browser console, so the cause can be found.

## 4.1.2 — 2026-10-21

### Fixed: the auto-deploy stopped deploying
- **The Rust hub's own data files are gitignored now** (`bridge/front-sessions.json`, `device-locks.json`,
  `api-keys.json`, `work.json`; also the auto-deploy's `bridge/.rolled-back` note). Without that, the live checkout
  looked as if it had uncommitted changes as soon as the Rust hub first ran there (4.1.0, 07:17), and the auto-deploy
  refused every deploy after it ("live checkout has uncommitted changes"). That is why 4.1.1 did not come up.
  (On the server, those four names were listed in the live checkout's own `.git/info/exclude` once, by hand, to let
  this version through.)

## 4.1.1 — 2026-10-21

### Fixed (found by Gee's Windows check, board T-133)
- **One account, several devices.** The device lock allowed exactly one device, and the app and the browser on one PC
  are two devices (the app's window keeps its own storage). Now an admin's **"Allow one more device"** (Admin →
  accounts) lets the next sign-in from a new device join the account's devices (up to 5). The devices it has keep
  working, and any further new device is refused again until an admin allows it. "Reset lock" still clears all of them.
  The account list says how many devices an account has and whether one more is allowed. Existing locks keep their
  device (`device-locks.json` gets an optional `devices` list and `allowNext`; the JavaScript fallback the same in
  state.json). The refusal now says to ask for one more device.
- **A hard reload no longer signs you out or leaves a blank page.** The page started before its last scripts had
  loaded (on a hard reload they come over the network), and any error while opening signed you out, which could then
  fail too and hide everything. It now starts once every script is there. A session ends only when the hub says it is
  over (401); any other failure keeps you signed in and shows what went wrong.
- **Old shortcuts go:** installing the app (`vr-desktop --install`) or the native AI host (`vr-host install`) on
  Windows removes the earlier JavaScript host's separate "Start / Stop VibeRoller AI Host" shortcuts from the Desktop
  and the Start menu.
- **The new Rust hub really runs after a deploy.** The auto-deploy builds the native programs after the deploy is
  verified, and nothing restarted the hub after that, so a deploy ran the new JavaScript with the previous `vr-hub`
  until some later restart. A finished native build now asks the hub for a graceful restart (`bridge/.restart`: it
  waits until nobody is busy, nobody is signed out).

### Tests
- R168 (several devices: admins only, the next new device joins, the first keeps working, a further one is refused);
  R6 follows the new message.

## 4.1.0 — 2026-10-21

### Security: only Gee works on Gee's computer (Sponge's decisions on the T-135 assessment)
- **An AI host's computer is only for its owner.** PC tools reach a host only when the hub registered it to that
  person (Settings → My AI hosts, or "Run on this PC" in the native app), never for the name a host says it belongs to
  in its hello (an older config-listed host could name anyone). PC tools elsewhere are unchanged: Work on the server,
  the native app working on your own computer, the browser's open folder.
- **An older AI host (no relay) would run a whole Work run on its own computer:** that is now refused for anyone but its
  owner, with a message saying why (its AI through the API is unchanged: a sandboxed answer, no tools). A current host
  only lends its AI; the run happens on the server.
- **A custom endpoint (`openai`) or a Claude config folder on the hub's machine (`claude-cli`)** can be added only by an
  admin signed in on an AI host of their own (online then), and counts only while its owner is an admin. An Anthropic
  API key works for everyone, as before.
- Both hubs (the JavaScript fallback too).

### Tests
- R112 (providers: the admin with a host of theirs makes them; no host / not an admin → refused), R152 (Work on an
  older host's computer → refused for others).

## 4.0.0 — 2026-10-21

**MAJOR: the Rust hub is the hub** (board T-134, item 4). Every request is answered by `vr-hub` now; the JavaScript
hub (`bridge/server.js`) answers nothing while it runs and stays behind it only as the **fallback**: `run.js` starts it
alone when there is no `vr-hub` binary, when the Rust hub keeps failing, or with config `rustHub: false`. The pieces
that changed on purpose, and stop working the old way:

### Changed: the last routes and restarts move to the Rust hub
- **`/api/status` and `/api/version`** are answered by the Rust hub. From this machine `/api/status` names the Rust
  hub's own process (`pid`), whether anyone's AI is busy (Work, the API, Unity, Unity in the chat), `supervised`,
  version and commit; from outside (the tunnel, or nginx with `behindProxy`) still only `{ ok, hub }`.
- **Restarts are the Rust hub's.** It watches `bridge/.restart` (`node bridge/restart.js`, the admin's restart): as
  soon as nobody is busy it ends with 75, and `run.js` starts **both** hubs again on the new code (the JavaScript one
  first, then the Rust one, which reads the new module list). Before, only the JavaScript hub restarted, so a new
  `vr-hub` build waited for a full stop. Behind the Rust hub the JavaScript hub no longer acts on the flag.

### Changed: Downloads are the Rust hub's, native only, and need an account
- **The Download panel hands out only the native programs:** the VibeRoller app and the native AI host (Windows +
  Linux, with your own host's config when asked). The earlier script packages are gone: the browser launcher
  (`bridge/desktop/`) and the Node.js AI host zip. `bridge/downloads.js` is removed; the JavaScript hub on its own says
  downloads come from the Rust hub (503).
- **Nothing is public any more.** `/api/pub/versions`, `/api/pub/native[/file]` and `/api/pub/*.zip` answer 410 to
  everyone (signed in or not) with what to do instead; the list, the zips and the app's release need a session (401
  without one).
- **The desktop app updates itself with an account:** the hub's page hands the app its session when you sign in
  (`set_session`, new and allowed for the hub's page) and clears it when you sign out. The app keeps it in its private
  `desktop.json`, sends it only to its own hub (changing the hub address drops it), asks the new
  `/api/native?app=vr-desktop&target=` with it, and its background update waits until someone is signed in. Against a
  hub before 4.0.0 it still uses the old public route. **An app installed before 4.0.0 cannot update itself from a
  4.0.0 hub** (it only knows the public route): download it once more from the Download panel.
- AI hosts are unchanged: they update over their own authenticated link (`/node/update*`).
- The Download panel says so when nothing is built on the hub yet.

### Changed: Unity in the chat queues like an agent, one conversation at a time
- **Every call is answered in its own conversation's turn.** Before, a call that came while she was busy took the one
  waiting slot (a second call replaced the first, so one was never answered), and her next turn read **every** unseen
  call across all conversations and answered them all in one place, so a private message to her could be answered in a
  channel or to someone else.
- Now, like a standard agent's message queue: calls that come while she is busy wait in order; more calls in a
  conversation that is already waiting merge into that conversation's next turn; each turn sees only its own
  conversation's messages (# Everyone, one channel / group, or one person's private messages). What she has answered is
  kept per message in `Chat/agent-seen.json` (`lastSeq` + `handled`; an older file is read as before).
- **On a Claude brain each conversation has its own session** (`Chat/agent-state.json` `threads`; # Everyone keeps
  `sessionId`), so nothing said in one conversation is remembered in another. Sign-in greetings go to # Everyone's turn
  only.
- No per-person quotas (decided 2026-10-21).

### Tests
- R142 (the Download panel), R143 (a host's config, now from `/api/hosts/config`), R155 (updates need an account; it
  replaces the script packages' installer checks) and R167 (the native downloads, `--rust-hub` only) follow.

## 3.25.0 — 2026-10-21

### Changed: Downloads and new projects in Rust behind the Rust hub (board T-134, toward 4.0.0)
- **The Download panel** (`modules/downloads.rs`): the same packages, READMEs, zip files and VERSION.txt fingerprints:
  - the native app and the native AI host (with your own host's config), and the earlier script packages
  - the public versions, native releases and Desktop packages
- **Starting or pulling in a project** (Work's header) is answered here; it runs `bridge/worktree.js new / clone` as
  that person, with their Forgejo token.
- The JavaScript hub now answers only `/api/status` and `/api/version`; it stays behind the Rust hub as the fallback.

### Security (board T-135, the assessment's findings in the Rust hub)
- **The hub can no longer be crashed by an AI host's answer.** A status code outside 100–599, or a header that is not
  valid HTTP, made building the answer fail, which aborted the whole process. Such status codes now become 502 and
  such headers are dropped.
- **Unity's engines come only from a host in the hub's config or one owned by a hub admin**, unless config
  `assistant.node` names one. Before, any person's host that announced an engine could receive everyone's Unity
  prompts, voice and pictures.
- **A Work run's tools (and Unity's Claude brain's) answer only as its own person.** The MCP key is now per account
  (an HMAC of the hub's secret and the account), so a run that names another account in `x-mills-account` is
  refused.
- **`team_say` follows the person's own posting rules:** timeouts, permissions, the new-member wait, slowmode.
- Read receipts and typing only for conversations that exist; nothing made up is kept.
- **Unity's `read_page`** connects to exactly the address it checked (no DNS rebinding) and reads at most 2 MB within
  20 s.
- **Calls to an AI provider's endpoint follow no redirects.**
- **The page server** refuses paths with a drive letter or a stream name: on Windows, `C:…` would name a path outside
  `web/`.
- **A linked Linux account's `~/uploaded_files`** is used only when it is a real folder in that home, not a link;
  otherwise uploads go to the hub's own area for them. A link could have pointed at the hub's own data.
- A person owns at most 100 group conversations.

## 3.24.0 — 2026-10-21

## 3.24.0 — 2026-10-21

### Changed: the page, profiles and the admin's accounts in Rust behind the Rust hub (board T-134, toward 4.0.0)
- **The page itself** (`modules/site.rs`): web/ as files, and index.html with this hub's values filled in, every script
  and stylesheet asked for under this build's name, and its CSP pinned to the sha256 of its own inline scripts.
  `/api/check` is answered here, and every API answer names the build (X-VR-Version).
- **Profiles, prefs and pictures** (`modules/profile.rs`): the same rules and the same places (`state.json`
  "profile" / "prefs", `Avatars/`).
- **The admin's accounts** (`modules/admin.rs`): the list, making an account with its first password, deleting one
  (moved to `profiles-deleted/`), Stop, and asking for a restart. **My Forgejo** (connect / disconnect) is here too.
- **Still in the JavaScript hub:** `/api/status` and `/api/version`, the Download panel, and adding a project. It
  re-reads the accounts when the Rust hub makes or deletes one.

## 3.23.0 — 2026-10-21

### Changed: Unity in Rust behind the Rust hub (board T-134, toward 4.0.0)
- **Unity's section** (`native/crates/vr-hub/src/modules/unity*.rs`) runs in the Rust hub:
  - her local brain (Ollama with native tool calls, through the AI host that lends her engines or here), or a Claude
    brain (`claude -p` with her tools through `bridge/assistant-tools-mcp.js`, with this hub's own secret)
  - every tool: time, place, weather, news, web search, public pages only, YouTube, pictures (make / edit / inpaint),
    the calculator, tasks, calendar, reminders, memory, knowledge, and the PC / code tools done by the person's browser
  - yes / no for destructive tools, voice and speech, uploads, and the reminders' notices
- **The same files:** `<profile>/assistant.json` and `Images/`. Her brain choice and Claude session, and the admin's
  PC / code switches, move to `<profile>/unity.json` (taken over from `state.json`); the admin's switches are answered
  here too.
- **The team chat's Unity** runs in the Rust hub as well: called by @<her name> or a private message, on her local
  brain or a Claude model. She greets people at sign-in, with their local time, weather and news.
- The JavaScript hub's Unity starts no engines and fires no reminders while the Rust hub runs her.

## 3.22.0 — 2026-10-21

### Changed: the Rust hub is on by default (board T-134, item 3)
- `bridge/run.js` puts the Rust hub (vr-hub) in front whenever its binary is there; every module passes the whole suite
  through it. Config `"rustHub": false` (or `{ "enabled": false }`) runs the JavaScript hub alone. Without the binary,
  or when it keeps failing, the JavaScript hub runs alone by itself, as before.
- The suite's normal run sets `rustHub: false`; `--rust-hub` runs it through the Rust hub.

### Fixed: the native programs on Windows (board T-133, what the server can check)
- **The desktop app starts on a clean Windows.** `vr-desktop.exe` loads `WebView2Loader.dll` from its own folder (the
  windows-gnu build cannot link it in). The DLL now ships with it: `native/dist`, the native app download, and
  `vr-desktop --install` all include it. Without it, the app stopped with "WebView2Loader.dll was not found".
- **The host config is private on Windows too.** `vr_core::settings::private` locks a file's ACL on Windows when
  someone else could read it: the person, SYSTEM and Administrators only, named by their well-known ids. Before, this
  only happened on Linux. The native host now locks `ai-node.json` by itself, as it does on Linux, and `vr-host
  lockdown` works the same way on Windows in any language. This matters for an install in `C:\VibeRollerAIHost`,
  which every user can read.
- **The AI host service:**
  - Reinstalling it waits for the old one to stop and to be gone before making it again. A host finishing its work
    took longer than the fixed 2 s wait, and the new service failed with "marked for deletion" (1072).
  - Its access list is set without an audit part, which an elevated prompt cannot set; that would refuse the whole
    change and leave non-admins unable to start or stop the service.
- Checked on the server: the Windows programs load only Windows' own DLLs and the UCRT (no MinGW runtime);
  `vr-desktop.exe` and `vr-hostw.exe` are window programs and `vr-host.exe` / `vr-hub.exe` console ones. The
  hands-on list for a Windows PC is the team note `how-to/windows-verification.md`.

## 3.21.0 — 2026-10-21

### Changed: the chat in Rust behind rustHub (board T-131, the last of its modules)
- **vr-hub's chat module** (`native/crates/vr-hub/src/modules/chat.rs`, `guilds.rs`, `perms.rs`) runs the chat itself:
  - messages: send, edit, delete, voice clips, attachments, end-to-end encrypted as before (ciphertext only; each
    reader gets only their own key slot)
  - keys, receipts, typing, presence, and the room's long poll
  - servers, channels and groups, with Discord-style roles, permissions, invites, moderation, the audit log and pins
  - calls (the encrypted set-up relay; who is in which call)
  - the admin's "clear the chat"
  - the team tools of Work runs
- **Unity's side**: her key stays in `Chat/agent-key.json`. The Rust hub opens what she is sent, sees when she is
  called, and posts what she answers. Her brain stays in the JavaScript hub, which hears she was called and reads what
  she was sent through `/__front/chat/*`. What she has read is kept in `Chat/agent-seen.json`, taken over from
  `agent-state.json`.
- Same files: `Chat/messages.jsonl`, `keys.json`, `seen.json`, `guilds.json`. Nothing moves and nothing is
  re-encrypted.
- **What stays in the JavaScript hub:** profiles, prefs and pictures (it tells the Rust hub when one changes, so the
  member list reloads), and Unity's section.

## 3.20.0 — 2026-10-21

### Changed: the external API and the AI providers in Rust behind rustHub (board T-131)
- **vr-hub's api module** (`native/crates/vr-hub/src/modules/openai.rs`) answers `/v1` itself:
  - `/v1/models`, `/v1/chat/completions` (OpenAI) and `/v1/messages` + `count_tokens` (Anthropic), streamed or not, in
    the same shapes, errors and limits (one answer at a time per key; the caller hanging up cancels it)
  - every provider kind: this hub's Claude and a Claude Code login (`claude -p` in `Api/sandbox`), an Anthropic key, an
    OpenAI-compatible endpoint, and an AI host's endpoint, Claude login, Codex login or legacy run
- **Settings → API access** (`/api/apikey*`): the keys move to `bridge/api-keys.json` (sha256 only), taken over from
  each account's `state.json` the first time.
- **Settings → AI providers** (`/api/providers*`, `bridge/providers.json`): a removed provider is dropped from
  everyone's Work and API choice.
- **Settings → My AI hosts** (the list): answered here too. `/api/check` gets the providers and the default provider
  from here.
- The data lists (`handoff.js`, the auto-deploy's snapshots, `pack-data.sh`) include `bridge/api-keys.json`.
- R111 reads the key's record from `api-keys.json` when the Rust hub is in front.

## 3.19.0 — 2026-10-21

### Changed: Work runs in Rust behind rustHub (board T-131)
- **vr-hub's work module** (`native/crates/vr-hub/src/modules/work.rs`) runs Work itself:
  - Claude Code in the person's own worktree, with the same arguments, instructions (the worktree law with the latest
    background check), tools, model, provider and resume rules
  - its stream to the page, Stop, and the PC tools relayed to the person's own AI host or their browser
  - the board, notes and team tools of a run, here and on a host
  - a host's shared login relayed for a run (`/llm/<token>/…`)
  - the legacy runs on a host
- Around it:
  - the model / provider choice, the project, `/api/usage` (`usage.json` is written here now)
  - shared Work sessions (watch-only), the Work conversation + queue (`work-current.json`), past sessions (`Archive/`)
- **Each person's Work state** moves to `<profile>/work.json` (provider, model, Claude session, context, project, last
  run), taken over from their `state.json` the first time.
- **What stays in the JavaScript hub:**
  - Settings → AI providers (`providers.json`, which the Rust hub reads) and adding projects
  - the chat, presence, calls and servers, which the team tools still reach through it
- **How the two hubs share the rest:**
  - The Rust hub patches `/api/check`, `/api/status` (busy) and the admin's account list with what it knows.
  - It tells the JavaScript hub its shared sessions and when a run is going, so a requested restart still waits for it.
  - It runs the admin's Stop and stops runs on account deletion.
  - `run.js` gives it the Node.js path for the tools of its runs.
- `deploy/pack-data.sh`, the auto-deploy's snapshots and `handoff.js` include `bridge/work.json` and
  `bridge/work-current.json`. `pack-data.sh` also now includes the Rust hub's files and the AI host registry.

## 3.18.0 — 2026-10-21

### Changed: the AI hosts run in Rust behind rustHub (board T-131)
- **vr-hub's aihosts module** (`native/crates/vr-hub/src/modules/ai_hosts.rs`) answers every AI host itself: the
  registry (`bridge/ai-hosts.json` + config `aiNodes`), the protocol (`/node/hello`, the long poll with the hub's
  heartbeat, reply, stream, events, the JavaScript host's file updates, the native host's signed binary updates), the
  sessions file (`bridge/ai-host-sessions.json`: a restart keeps every host's session), the up / down log, the probe of
  older hosts, caps from the usage headers, and Settings → My AI hosts (new, rename, remove, share, config). The same
  files and the same protocol, so a host (JavaScript or native, old or new) cannot tell which hub it talks to.
- **The JavaScript hub uses them through the front** (`bridge/ai-nodes-front.js`, the same calls as
  `bridge/ai-nodes.js`): it knows what the hosts offer from the front's snapshot (sent on every change and every 5 s;
  online counted from each host's last poll), and carries Unity's engine calls, a host's endpoint and login calls (the
  answer streams back as it comes), Work runs on a host (one line per event) and PC tools through `/__front/ai/*`. A Work
  run's tools on a host (`/node/job/<id>/internal/<tool>`) come with the account the front checked the job's token for.
  A host's usage windows reach the JavaScript hub's `/api/usage` as they come.
- `native_releases.rs`: the release manifests and their signatures in Rust (the same text, Ed25519 + the host's HMAC).
- Without rustHub (or with an older vr-hub that does not name `aihosts`) nothing changes: `bridge/ai-nodes.js` runs them.

## 3.17.0 — 2026-10-20

### Added: the Downloads are the pre-built native programs (board T-134, items 1 + 2)
- **Download panel**: first the native programs, pre-built for Windows and Linux, nothing to install first:
  - **VibeRoller (native app)** (`app-windows` / `app-linux`): vr-desktop + the AI host it switches on. It runs from its
    folder or installs itself (`vr-desktop --install`, or Tools → Settings → Install on this computer: the standard
    folder with Start-menu + Desktop shortcuts / an application-menu entry and `~/.local/bin/viberoller`, an earlier
    install updated). Public, like the Desktop packages.
  - **VibeRoller AI Host (native)** (`host-windows` / `host-linux`): vr-host (+ vr-hostw), with the person's own host
    config inside from Settings → My AI hosts (those buttons now hand out the native host when this hub has one built).
    `vr-host install [--boot …]` installs it or updates an earlier install (the JavaScript host's folder too, taking its
    ai-node.json and settings over; a running host there is stopped first and started again on the new version).
  - The earlier script packages (browser launcher, the Node.js AI host) stay listed after them, for machines using them.
- **Built automatically**: the server's auto-deploy rebuilds the native programs in the background whenever the running
  version's `native/` differs from what `native/dist` was built from (a systemd unit of its own, so it outlives the
  timer's run; a failed build keeps the last one; `native-build.log`). `native/build.sh` finds the server's own Node.js
  so every build carries the release key. The data snapshots now include the Rust hub's files and the release key.
- R167: the panel lists them, the zips hold the binaries (and the host config), the app is public and the host is not,
  `/api/pub/versions` names them, and the downloaded host installs itself.

## 3.16.0 — 2026-10-20

### Changed: files run in Rust (board T-131, module 3 of 7)
- Behind `rustHub`, vr-hub's files module (`modules/files.rs`) answers the chat's attachments (ciphertext only, the
  uploader's own seal), Work uploads (into the linked Linux account's `~/uploaded_files/<date>/`, else the hub's area),
  and the Files library (list with files put there by hand, delete, edit in place, executable on / off), plus the link
  status of the Linux account; the same files as before (`Chat/files`, `uploads-index.json`, the uploads).
- The chat (still in the JavaScript hub) asks it to attach a message's files (the message number is taken first, so a
  message sent meanwhile cannot take it), tells it when messages are gone or the chat is cleared, and answers whether
  someone may read a message before one of its files is handed out (`/internal/front/may-read`). A Work run's attached
  files get their paths from it. The Rust front also checks a page's origin and adds the hardening headers itself.

## 3.15.0 — 2026-10-20

### Changed: the board and team notes run in Rust (board T-131, module 2 of 7)
- Behind `rustHub`, vr-hub owns the team board (`modules/board.rs`: tasks, claims, revisions, month-end aware repeats,
  the calendar, the long poll; the same `Board/board.json`), its Forgejo issue mirror (`modules/forgejo.rs`: outbound
  as the person who made the change, the poll as "Forgejo"; the personal tokens stay in the JavaScript hub, which it
  reads), and team notes (`modules/notes.rs`: the same `Notes/` folder and `.index.json`). Due dates are read in the
  machine's local time, as before.
- Claude's board and notes tools (Work runs) still come in through the JavaScript hub, which checks the run's token and
  hands them over (`/__front/board/tool`, `/__front/notes/tool`, with the front's secret); a notes change wakes the
  pages' chat poll there (`/internal/front/room-changed`), and the notes' version rides along on every request.
- `bridge/run.js` asks vr-hub which modules it answers (`vr-hub --modules`), and the JavaScript hub leaves those alone
  (its own Forgejo mirror stays off). While moving house (`bridge/.maintenance`) the Rust hub answers changes "moved"
  exactly as the JavaScript one does; `handoff.js pack` takes the Rust sign-in module's files along.

## 3.14.0 — 2026-10-20

### Changed: accounts & authentication run in Rust (board T-131, module 1 of 7)
- Behind `rustHub`, vr-hub's auth module (`native/crates/vr-hub/src/modules/auth.rs`) does sign-in (scrypt, the same
  auth.json; the per-address limits; the same cost for unknown names), the device lock, the 12-hour sessions (sha256
  only, `bridge/front-sessions.json`), sign-out, the session gate for every request, and the admin's new password /
  unlock / sign-out; the admin list gets its locks and sign-ins from it. On its first start it takes over the
  JavaScript hub's sessions and device locks, so nobody is signed out or loses their lock.
- The JavaScript hub, behind it, trusts who a request belongs to (`X-VR-User`) only with the front's secret and never
  reads a token itself; a caller's own `X-VR-User` / `X-VR-Front-*` headers are dropped at the front. A sign-in there is
  told to the JavaScript hub (`/internal/front/signed-in`: the client details, Unity's greeting).
- Without `rustHub` nothing changes. The whole suite passes both ways.

## 3.13.0 — 2026-10-20

### Added: the Rust hub, in front (board T-130)
- **vr-hub** (`native/crates/vr-hub`): the hub in Rust, growing module by module in front of the JavaScript hub. With
  config `"rustHub": { "enabled": true, "backPort": 7779 }`, `bridge/run.js` starts it on the hub's port (what nginx
  already sends the portal to, so nginx needs no change) and the JavaScript hub behind it on 127.0.0.1:backPort. It
  answers the modules it has and passes everything else through, streamed both ways (long polls, streamed answers,
  uploads up to 32 MB); while the JavaScript hub restarts, requests wait up to 30 s for it instead of failing. If its
  binary is missing or it fails three times in a minute, the JavaScript hub takes the port back by itself.
  `/__vr-front` names it and its modules; every answer carries `X-VR-Front`.
- **The whole regression suite runs through it**: `node bridge/test/regress.js --rust-hub` (all checks pass; R166 checks
  it is really in front and the JavaScript hub sits behind it). Off by default until the modules have moved (T-131).

## 3.12.0 — 2026-10-20

### Added: VibeRoller Desktop, native (Rust + Tauri), Linux and Windows (board T-128, T-129)
- **Two windows, two levels of trust** (`native/crates/vr-desktop`): the main window shows the hub's own page (always
  current; it updates itself as in the browser) and may ask the app only harmless things: its version, desktop
  notifications, the unread count on the tray / taskbar, the Tools window, links in the browser, the AI host switch.
  The **Tools** window is the app's own page (built in, never loaded from the network) and is the only one allowed to
  touch the computer, so a script injected into a hub page could not.
- **Tools**: the local projects folder (browse, edit, create, delete; no browser permission prompts), an integrated
  terminal (a real pseudo-terminal: bash / pwsh / cmd), build & run with saved buttons per project, git with Forgejo
  sign-in (the token goes along per call, never into the repo), the local API proxy (`http://127.0.0.1:<port>/v1` →
  the hub's OpenAI- and Anthropic-compatible API with the person's key; this computer only; web pages refused), the AI
  host switch (start / pause / resume / stop / update / start at boot) and the app's updates.
- **Hosting this PC's AI from the app**: Settings → My AI hosts → **Run on this PC** (shown in the desktop app) fetches
  that host's settings (`POST /api/hosts/config`, the owner only) and the app's own native AI host runs it.
- **Tray** (show, Tools, AI host on / pause, updates, quit), close to tray, one instance at a time, native
  notifications for chat messages while the window is in the background.
- **Updates** signed with VibeRoller's release key: `/api/pub/native` + `/api/pub/native/file` (public: the binaries hold
  nothing private and are signed); checked at start and every 6 hours, applied when no terminal or run is open.
- **Built without root**: WebKitGTK / GTK development files from Debian's own packages in a user-space sysroot
  (`native/tauri-env.sh setup`); Windows needs nothing more. `native/smoke-desktop.sh` starts the Linux app headless
  (Xvfb in a user namespace) and checks both windows; R165 runs it against the test hub.
- The page: `NATIVE` (window.__TAURI__ inside the app), the Tools tile, Run on this PC, the badge and notifications;
  its CSP lets the app's IPC through (`ipc:` / `http://ipc.localhost`, which no browser has).

## 3.11.0 — 2026-10-20

### Added: the native AI host (Rust), pre-built for Linux and Windows (board T-122 … T-127)
- **Toolchains on the server, all user space** (T-122, T-123): rustup / cargo / rustc 1.99.1 in the hub account, the
  `x86_64-pc-windows-gnu` target, and llvm-mingw (`~/.local/opt/llvm-mingw`) as the Windows linker, its compiler-rt
  and libunwind standing in for libgcc. `sh native/build.sh` tests, then builds release binaries for both
  (`native/dist/<target>/` with `BUILD.json` + `SHA256SUMS`); `check` / `test` for less.
- **vr-core** (T-125): the AI host protocol + session envelope (HKDF + AES-256-GCM), update signatures (HMAC + Ed25519),
  the chat's end-to-end encryption exactly as the page does it (formats 3 / 4, files, own seals, voice clips), ai-node.json
  with the control app's keys, defaults and checks (unknown keys kept), and a hub API client. Checked against Node and
  WebCrypto both ways (R163).
- **vr-host** (T-126, T-127): the AI host and its heart & knife control app in one program: the same folder, settings,
  status file and control requests as the JavaScript host, so it can take that one's place and either control app works
  with either. Login relays (Claude / Codex) with token refresh, engines with the engine guard, OpenAI-compatible
  endpoints, streaming back, PC file tools, Work runs (with its own MCP servers for the PC / board / team tools), usage
  caps, the heartbeat and reconnects, pause, a terminal dashboard (ratatui) with every setting, start at boot (Windows:
  the VibeRollerAIHost service via the SCM, or sign-in through the window-less `vr-hostw`; Linux: system or user
  systemd unit), and `vr-host run`, which supervises it (restart on exit 3, back off after crashes, log rotation).
  The whole regression suite passes with it as the test host (`node bridge/test/regress.js --rust-host`); R164 runs it
  through set / settings / its MCP tools / status / an update / pause / stop.
- **Signed native releases from the hub** (`bridge/native-releases.js`): the binaries in `native/dist`, described in a
  manifest signed with VibeRoller's release key (Ed25519, `bridge/release-key.json`, gitignored, made on first use; its
  public half is built into every binary) and, for a host, with its own secret too. `GET /node/update/bin` +
  `/node/update/bin/file` for hosts that say `update-bin`; a host checks, self-tests, swaps in its new binary (the old
  one kept as `.old`), restarts on it and confirms it once it reached the hub; three starts that never reach it put the
  old one back.

## 3.10.0 — 2026-10-20

### Security (board T-132)
- **Sign-in sessions are stored as hashes**: `hub-state.json` keeps the sha256 of each session token, never the token,
  so a copy of the file (a backup, a data snapshot) signs nobody in. Sessions an older hub saved in the clear are hashed
  on the first start, so nobody is signed out by the update (a rollback to an older hub would sign people out once).
- **No inline script runs but the page's own**: the hub serves the page with its CSP's `'unsafe-inline'` for scripts
  replaced by the sha256 of each of the page's own inline scripts, so injected script and inline event handlers are
  refused by the browser. `worker-src blob:` lets the code sandbox (run_code) start its workers, which the old policy
  silently blocked. A static copy of the file with no hub in front still says `'unsafe-inline'`.
- **No more hanging requests**: every request runs through `handleRequest(...).catch(...)`, so a route that hands back a
  promise without awaiting it is answered when that promise fails, instead of leaving the connection open (the 3.8.0
  upload hang, fixed for every route at once).
- nginx rate limits: unchanged in the repo (`deploy/nginx-portal.conf` has them); installing them needs root. R162.

### Added
- **The Rust workspace** (board T-124): `native/` is one Cargo workspace (resolver 2, edition 2021, a small stripped
  release profile) with the shared `vr-core` library crate, its planned modules named after their JavaScript
  references; `native/target/` is gitignored and
  `native/` is published (the whitelist .gitignore). Not compiled yet: the toolchain (T-122) is not installed.

## 3.9.0 — 2026-10-20

### Added
- **Protocol adapters, both directions, one key** (bridge/openai.js; board T-120): `POST /v1/messages` takes
  Anthropic's Messages format (system and content blocks, `max_tokens`, `stream`) and is answered by whichever provider
  the key uses, an OpenAI-compatible endpoint or a host's local model too (Anthropic → OpenAI), in Anthropic's message
  shape, streamed as Anthropic's events (`message_start` … `content_block_delta` … `message_stop`), errors in
  Anthropic's shape. `POST /v1/chat/completions` (OpenAI format) keeps reaching every Claude (OpenAI → Anthropic).
  `/v1/models` answers in Anthropic's list shape when `anthropic-version` is sent; `/v1/messages/count_tokens` gives a
  rough count. The same `mills_…` key works as `Authorization: Bearer` or `x-api-key` on both. Text only: tool calls,
  tool results and pictures are passed on as text.
- **Files, complete** (board T-121): a Work file is edited in place (text up to 2 MB; Ctrl+S; its permissions kept),
  made executable or not (chmod), and sent to Work as it is ("Use in Work", or "Run in Work": Claude runs it and
  reports). Files put in `~/uploaded_files` outside VibeRoller (over SSH, by a Work run) show in the library too and
  open, download, edit, run and delete like the rest. `POST /api/library/save?id=`, `POST /api/library/exec`. R160, R161.

## 3.8.0 — 2026-10-20

### Added
- **VibeRoller accounts linked to the server's Linux accounts** (bridge/linux-accounts.js): sponge → sponge, gee → gee,
  mills → mills (config `linuxUsers` overrides; same name by default). Each person links once, as themselves, with no
  root: `sh /srv/apps/VibeRoller/app/deploy/link-linux-account.sh` (over SSH or by their AI agent there). It makes
  `~/uploaded_files` and lets the hub's Linux account read and write that folder only (ACLs; passing through the home
  folder, never listing or reading it); `… unlink` takes it back. Linked, their Work uploads land in
  `~/uploaded_files/<date>/<name>`: theirs to work on, edit and run in their own account (the shared projects stay in
  /srv/apps/sharedwork, which everyone's group reaches). Not linked: VibeRoller's own area, as before.
- **Files** (a menu tile; web/library.js, on the web and in the Desktop app): everything you uploaded, with its name,
  type and size, when, where it went (Work · project, or the chat conversation, and whether that message was deleted
  since), and when it was last opened or used; search, Work / Chat filter; Open (pictures, players, the text / code
  viewer), Download, Delete (for good), and for a chat file "Copy to my uploaded_files" (decrypted in the browser and
  uploaded as a normal file). The top says whether your server account is linked, and how to link it.
- **Chat files stay end-to-end encrypted, and stay yours:** the uploader's browser hands over the file's name, type and
  key sealed to the uploader alone (only their chat key opens it), so when a message is deleted its readers lose its
  files while the uploader keeps them in Files (marked "message deleted") until they delete them there. Clearing the
  chat works the same way. Files from older pages (no seal) still go with their message.
- Check R159; R158 follows (uploads in uploaded_files when linked).
- Modules: bridge/files.js 1.1.0, bridge/linux-accounts.js 1.0.0 (new), bridge/server.js 3.5.0, web/attach.js 1.1.0,
  web/index.html 1.8.0, web/library.js 1.0.0 (new), web/sw.js 1.0.3.

## 3.7.0 — 2026-10-20

### Added
- **Attachments, in the team chat and in Work.** A + button on both message boxes (drop and paste work too) attaches one
  or more files, pictures or videos (up to 24 MB each, 20 per message), with or without text.
  - **Chat, end-to-end encrypted like the messages:** each file is encrypted in the browser with its own key; only the
    ciphertext reaches the hub (`POST /api/room/file`), and the file's name, type, size and key travel inside the
    encrypted message body. The hub only learns which file ids a message has, to let exactly that message's readers
    fetch them (`GET /api/room/file/<id>`); a file is sent once, never with someone else's upload; deleting the message
    (or clearing the chat) deletes its files; uploads never sent are removed after a day. Store: Chat/files/ (part of
    the data snapshots and the data move). Unity, when called, sees the files' names.
  - **Work:** files go to the person's own upload area on the hub (bridge/profiles/<user>/uploads, kept 30 days); their
    Claude gets the paths and read access to that folder (`--add-dir`), so it reads pictures, code and documents. The
    message carries its files as a marker block at its end, so the queue, the saved conversation and Past sessions
    keep them with no change. `GET /api/work/file/<id>` serves them to their owner only.
  - **How they show (web/attach.js), by what they are:** pictures inline (click for full size), video and audio with a
    player loaded on play, text / markdown / code in a viewer (markdown rendered, code highlighted for its language),
    and anything else (binary, unknown) only its name and size with Download: never opened, shown or played. Text is
    text only when it really is (no NUL byte, valid UTF-8), whatever its name says. Files are never served with a type a
    browser would run (octet-stream, nosniff, attachment).
- **Auto-scroll is a setting** (Settings → App settings), off by default: a conversation (Work, the team chat, Unity)
  follows new content only while you are at its bottom; with the setting on it always follows, until you scroll up,
  which switches the setting off by itself. When something new arrives below while you read further up, a "Newest"
  button sticks to the bottom and takes you there. Your own messages and opening a conversation still go to the bottom.
- Check R158.
- Modules: bridge/files.js 1.0.0 (new), bridge/server.js 3.4.0, web/attach.js 1.0.0 (new), web/index.html 1.7.0,
  web/sw.js 1.0.2.

## 3.6.0 — 2026-10-20

### Added
- **The heart & knife control app** (bridge/vr-host.js; Windows and Linux, in a terminal). One app replaces the separate
  heart (start) and knife (stop):
  - a live dashboard: running (process, service, how it starts at boot), connected to the hub (since when, the last
    heartbeat, the hub's version), in use (answers in flight, Work runs, the last job), updates (automatic or not, the
    last one), who may use each provider, the engines, a security summary;
  - [s] start, [x] stop, [r] restart (Shift = at once; otherwise as soon as nothing is in flight), [p] pause / resume
    (the old knife), [u] update now, [l] the log;
  - [c] every setting, written to ai-node.json and applied by the host at its next quiet moment: the hub's address
    (https only), the token (never shown), automatic updates and their timing, the reconnect delay, every provider (on
    / off, shared with the hub's **team group** and / or **named users**, 5-hour / 7-day caps), Unity's engines on / off,
    and the security switches (Work on this machine, PC file access, a plain-http hub, the engine guard), plus locking
    the config file down;
  - [b] start at boot: the service from boot (Windows: administrator once; Linux: sudo once), at sign-in / a user
    service (no administrator), or off;
  - the same as commands for scripts: `node vr-host.js status | start | stop | restart | pause | resume | update | log
    | boot … | settings | set <setting> <value>`.
  - Windows: host-tools\build.cmd builds the service and ONE shortcut, "VibeRoller AI Host" (a heart & knife icon), on
    the Desktop and in the Start menu, and removes the old heart / knife programs and shortcuts. Linux:
    host-tools/viberoller-host-control.sh; start-viberoller-host.sh / stop-viberoller-host.sh call the app. An
    installed package (VibeRoller AI Host.cmd / viberoller-ai-host.sh) opens the control app; a fresh install starts the
    host in the background and opens it.
- **The host reports and obeys:** ai-node.status.json (running, connected, heartbeat, in flight, paused, last update,
  last error), and stop / restart requests within 2 s (gently: after what is in flight, unless "now").
- **Settings the host honours:** `update.quietMinutes` / `update.checkMinutes`, `reconnect.maxDelaySeconds`, a provider's
  `enabled` and `shareWithGroup`, `enginesOff`, `security.*`. Older ai-node.json files work unchanged (safe defaults).
- **Sharing with the team group:** a provider shared with "@group" is open to the hub's team group (config hostGroup),
  whatever names are in it; named users besides. New hosts made in Settings share with the group by default.

### Security
- The host refuses a hub over plain http on another machine (the token would cross the network in the clear) unless
  `security.allowInsecureHub` is set; ai-node.json is made readable by its owner only (Linux / macOS; the control app
  locks it down on Windows).
- **Engine guard:** the hub may use a host's engines, never manage them: only GET / POST, no model delete / pull / push
  / create / copy, no `..` or `//` in paths (`security.engineGuard: false` lifts it).

### Changed
- Check R157; R136, R154, R156 follow.
- Modules: bridge/ai-node.js 3.3.0, bridge/ai-nodes.js 2.5.0, bridge/downloads.js 1.2.1, bridge/vr-setup.js 1.1.0,
  bridge/vr-host.js 1.0.0 (new).

## 3.5.0 — 2026-10-20

### Changed
- **The web page updates itself, with nothing for anyone to do.** It checks the hub's build every 5 minutes
  (`/api/version`) as well as on every answer (`X-VR-Version` now names the build: version + commit). When the build
  changed, it no longer waits for you to clear what you are typing:
  - it waits only for what finishes by itself or cannot be saved for you: a reply still streaming in (Work, Unity), a
    call, a file or a note with unsaved changes (it says so once);
  - it saves the session: the Work conversation and its queue on the hub (only when Work was opened in that tab, so an
    empty one never overwrites the saved one), and in the tab what you were typing in Work / Chat / Unity and the screen
    you were on;
  - it hard-reloads with every cache busted: the service worker's caches are emptied and the worker updated, the page
    is loaded under the new build's name, and the hub now serves every script and stylesheet under the build's name too
    (the page itself with no-store); then it restores the screen and the drafts and says it updated.
- AI hosts look for updates every 5 minutes (was 10), besides after every hello and when the hub's heartbeat names a new
  version. The hub's auto-deploy already checks Forgejo every minute; an installed Desktop checks at every start, and
  its window is this same page, so it follows every 5 minutes too.
- Check R156; R154 adjusted.
- Modules: bridge/server.js 3.3.0, bridge/ai-node.js 3.2.1, web/index.html 1.6.0, web/notes.js 1.0.2.

## 3.4.0 — 2026-10-20

### Added
- **Every download is its own installer, launcher and updater** (Windows and Linux), installed or portable:
  - **Vibe Roller AI Host**: run `VibeRoller AI Host.cmd` / `viberoller-ai-host.sh` (bridge/vr-setup.js). It finds earlier
    installs (the standard folder, the VibeRollerAIHost service's folder, C:\VibeRollerAIHost / ~/VibeRollerAIHost, a
    running host's lock) and updates them, keeping that machine's ai-node.json, logs and settings; a running host
    switches at its next quiet moment, and if an older launcher lets it stop, the setup starts it again. A launcher that
    is running is swapped at its next start, never under it. A host that runs from a git checkout (Gee's PC) is never
    overwritten: it updates itself. Nothing installed → Install (standard folder) or Portable. `--install`,
    `--portable`, `--update`, `--status`, `--yes`. The hub's host updates now include vr-setup.js too.
  - **Vibe Roller Desktop**: run `VibeRoller Desktop.cmd` (PowerShell, bridge/desktop/vr-desktop.ps1) or
    `viberoller-desktop.sh` (bridge/desktop/). It finds earlier installs (its folder, its shortcuts / the launcher older
    packages put in ~/.local/bin) and updates them; nothing → Install (with Start-menu / Desktop shortcuts or an
    application-menu entry) or Portable. Installed, it checks the hub at every start and updates itself when its
    package changed.
  - Every package carries **VERSION.txt**: the hub's version + a fingerprint of the package's contents, so installs
    update only when something in them changed. `GET /api/pub/versions` names them, public, as are the Desktop packages
    (`/api/pub/desktop-<os>.zip`: nothing in them but this hub's address); the AI Host packages stay signed-in only.
  - Check R155 (runs the Linux installers end to end; the Windows ones are checked in source). R142 adjusted.
- Modules: bridge/downloads.js 1.2.0, bridge/vr-setup.js 1.0.0 (new), bridge/ai-nodes.js 2.4.1, bridge/server.js 3.2.1.

## 3.3.1 — 2026-10-20

### Changed
- **Downloads have their own place.** Settings has a fourth tab, **Downloads**: Vibe Roller Desktop and Vibe Roller AI
  Host for Windows and Linux, how each one stays up to date (the page and the Desktop app follow the hub by themselves;
  an AI Host updates itself between answers, or by hand with `node ai-node.js --update`), the version the downloads
  contain, and installing the chat app from the browser (moved here from Connections). Work's side bar has a
  **Download** button right above Settings, like the menu; every Download button opens Settings on that tab (the
  separate Download pop-up is gone). The AI Host's description now says what it shares (its logins, local models,
  Unity's engines, never the computer). Check R142 (extended).
- Modules: web/index.html 1.5.1, web/app.js 1.1.1.

## 3.3.0 — 2026-10-20

### Fixed
- **Going live no longer cuts off Gee's AI host.** The hub kept each host's session (what it offers + the session key)
  in memory only. Every restart or deploy forgot it, while the host, still holding its key, kept polling and never said
  hello again: its Claude vanished for everyone until someone restarted the host by hand, usually as Gee was leaving.
  Sessions are now kept in `bridge/ai-host-sessions.json` (gitignored) and restored at start, so a host carries on
  through a hub restart as if nothing happened. This works with the host Gee runs today; nothing to do on his side. If
  a session is missing anyway, the hub asks the host to say hello again (428 for hosts from this version, 401 for older
  ones, which re-hello on it). 1.x hosts, which never say hello, keep polling as before. Check R154.

### Added
- **Heartbeats both ways.** A host's poll is its heartbeat; every poll answer now carries the hub's (time, version,
  commit). The hub logs each host going down / coming back with how long it lasted; a host from this version logs the
  same about the hub, reconnects within 1-5 s, says hello again at once when asked, and looks for an update when the hub's
  version changes. `hub_status` (team tools) lists every AI host: up or down, since when, its version.
- **Hosts never restart in the middle of an answer:** a restart for new code or an update waits until nothing is in
  flight (relayed answers, engine and PC jobs, Work runs) and nothing came for 3 minutes (1 minute for new code).
  `node bridge/ai-node.js --update` asks a running host to update from the hub at its next quiet moment.
- **The page updates itself.** Every signed-in answer names the hub's version (`X-VR-Version`); a page loaded on an older
  version reloads itself as soon as nothing would be lost (no reply streaming, no call, no unsent text, no open editor),
  and says so until then. The desktop app is the same page, so it follows too.
- **The versioning law** (LAWS.md): MAJOR for reworks of half the codebase or more, MINOR for moderate to large features
  or anything that might break, PATCH for fixes and small changes; lower numbers reset; bump before merging into
  develop, and bump again past a teammate who merged the same number. `worktree.js merge` refuses a branch whose version
  is not above develop's and names the right next numbers. Check R41 (extended).
- **The compatibility law** and **going live never cuts anyone off** (LAWS.md): Windows and Linux always; forwards and
  backwards compatible (a feature may be missing on the older side, everything else keeps working); everything updates
  itself.

### Changed
- Modules: bridge/ai-node.js 3.2.0, bridge/ai-nodes.js 2.4.0, bridge/server.js 3.2.0, bridge/team.js 1.1.0,
  bridge/worktree.js 2.2.0, web/index.html 1.5.0.

## 3.2.1 — 2026-10-19

### Fixed
- `LAWS.md` is published: the whitelist `.gitignore` (only listed top-level files go to the remotes) kept it out of
  3.2.0, so the server's Work runs did not get the worktree law. Check R150 now makes sure it is not ignored.
- The auto-deploy runs from a copy of itself (`deploy-state/autodeploy.run.sh`): a deploy rewrites `autodeploy.sh`
  while it runs, and sh reads a script as it goes (the 3.2.0 deploy ran half old, half new and skipped its log line).
  No module changed.

## 3.2.0 — 2026-10-19

The stability pass (board T-114). Sponge: worktrees that stay fresh, Forgejo used as intended, older AI hosts accepted,
everything updating itself, nothing breaking, data always kept and a bad version rolled back by itself.

### Added
- **The worktree law** (`LAWS.md`, a tracked laws file every Work run now reads next to the host's own `CLAUDE.md`):
  never build on a stale worktree. `node bridge/worktree.js check <user>` fetches Forgejo and says FRESH or STALE (exit
  3): behind Forgejo, or on the wrong branch. `update <user>` fast-forwards a clean one, merges Forgejo's integration
  branch into your own commits, or renews it. `renew <user>` makes a NEW worktree from Forgejo and carries over your
  commits, uncommitted changes and untracked files, then deletes just the old one. If something does not come over, the
  old one stays next to it with the reason. Its state is kept under `refs/viberoller/renewed/` either way, so nothing is
  ever lost. The hub checks worktrees in the background (when a run starts, on a project switch, every 5 minutes for
  anyone active) and fast-forwards a clean one by itself. Each run's instructions carry the latest finding. Check R150.
- **Forgejo is the source of truth.** `merge` merges onto Forgejo's develop in a scratch checkout and pushes FIRST; `live`
  merges Forgejo's develop into Forgejo's main and pushes it FIRST. A failed push changes nothing anywhere (it says to
  connect your Forgejo account if that is why). The server's auto-deploy then ships it. Local integration branches are
  brought level with Forgejo whenever the tool runs.
- **Deploys roll themselves back.** `deploy/autodeploy.sh` snapshots the hub's data before every deploy (the last 10, in
  `snapshots/`), then checks the hub really comes up on the new commit (`/api/status` now reports `version` + `commit`
  locally). A version that does not come up within 2 minutes of its restart (or never answers) is put back to the last
  good commit by itself. The team chat is told on the next start, and that commit is skipped until main moves on. A
  version whose scripts do not parse never starts. Data is never touched. Check R151.
- **AI hosts update themselves from the hub** (`GET /node/update`: the host's files with their sha256, signed with that
  host's own secret; `/node/update/file/<name>`). A host from this version on checks after every hello and every 10
  minutes, between answers only. It downloads what changed, verifies it, starts the new version once with
  `--selftest`, swaps it in with a backup and restarts on it. After 3 starts that never reach the hub it goes back by
  itself. A host running from a git checkout (Gee's PC) fast-forwards the checkout to the hub's commit instead.
  Launchers are never swapped while running. `"autoUpdate": false` in `ai-node.json` switches it off. Check R152.

### Changed
- **Older AI hosts keep working.** A host that does not say it can relay a login (every host before this version) is
  asked once with a relay job to an address no host forwards. A 3.x host refuses it (nothing leaves that machine); a
  2.x host does not know relays, and its Claude then runs Work the way it did then, instead of every run failing. Gee's
  3.0 host keeps working exactly as now; updating it once brings the self-updater, after which it never needs a hand.
- `start-node.bat` / `start-node.sh` start the host again on exit 3 (new code or an update) and 10 s after a crash; they
  used to close, so a host restarting itself for new code stayed off until someone started it.
- **Sign-in limits are per address.** The lockout used to be per username, so five wrong guesses from anywhere locked
  a teammate out; and flooding made-up usernames wiped every counter. Now: 5 failures for one username from one address
  or 20 from one address in total → that address waits 15 minutes; at most 3 sign-ins in flight per address; counters
  expire one by one. Pre-sign-in bodies are capped (16 KB for sign-in; too large → 413). `deploy/nginx-portal.conf` adds
  matching nginx rate and connection limits (applied when the site file is installed again; the hub's own limits work
  without it).
- The hub keeps running after an unexpected error in a callback (logged), and system errors are answered without
  server paths. The internal secret is compared in constant time.
- `worktree.js` run from inside a worktree reads the main checkout's `config.local.json` (it looked for worktrees in
  the wrong folder on the server).
- The regression runner drops a Work run's Forgejo login from its own environment (it leaked into the test hub's runs).
- Modules: bridge/ai-node.js 3.1.0, bridge/ai-nodes.js 2.3.0, bridge/server.js 3.1.0, bridge/worktree.js 2.1.0.

## 3.1.1 — 2026-10-19

### Fixed
- **AI hosts no longer drop off every minute.** When the server's checkout held commits that were not on Forgejo yet, the
  auto-deploy "deployed" the same commit every minute and restarted the hub each time, so every AI host (turned on with
  the heart) lost its line with "hub answered 502". `deploy/autodeploy.sh` now restarts only when Forgejo's `main` really
  has something new. The 3.1.0 commits that were made on the server are now on every remote. Check R149.

### Changed
- No module versions moved (deploy script and regression runner only).

## 3.1.0 — 2026-10-19

### Added
- **Direct messages are their own place in the chat.** The rail starts with a DM button (its own icon, with an unread
  count) above every server. It opens the Direct messages view: the one-to-one DMs, then the group DMs as DM channels
  ("New group DM"), and the key fingerprints. A server shows only its channels; groups are no longer listed inside it.
  Opening a DM or group DM from anywhere (a profile card, Unity in the member list) switches to that view, and the DM
  button reopens the last one. The rail also counts unread messages per server. Check R148.

### Fixed
- **Accurate 5h / 7d.** The exact numbers Anthropic sends with every call through an AI host's Claude (both the 5-hour
  and the 7-day window) now reach the usage bar: they are kept in `bridge/usage.json`, so a hub restart or deploy no
  longer forgets them, and Claude Code's own usage events, which often carry no number, no longer wipe them. A missing
  number counts as unknown instead of 0 %. The team tools' hub_status reads the windows of the AI the person actually
  uses (it read the hub's old login before). Nothing changes on the AI host. Check R147.

### Changed
- Modules: bridge/usage.js 1.2.0, bridge/ai-nodes.js 2.2.0, bridge/server.js 3.0.1, web/guilds.js 1.1.0,
  web/index.html 1.4.0, web/guild-settings.js 1.1.2.

## 3.0.0 — 2026-10-19

**An AI host shares its AI, never its computer.** MAJOR because a host stops doing what it did: it no longer runs Work
(Claude Code, files, tools) on its own machine.

### Changed
- **Work happens on the server, in sharedwork.** With a host's Claude chosen, Claude Code runs on the hub in the person's own
  worktree; only its model calls travel to the host (`/llm/<token>` on the hub, local only, a 12-hour token per person and
  provider), and the host adds its own login and forwards them to Anthropic. The login never leaves the host; no file on the
  host is read or written. Usage and caps come from Anthropic's own usage headers. Check R144 (rewritten).
- **The host refuses work and file jobs** unless its config says `"allowWork": true`; a host's `claude-cli` provider is
  shared as a login relay. Modules: bridge/ai-node.js 3.0.0, bridge/ai-nodes.js 2.1.0, bridge/server.js 3.0.0,
  bridge/providers.js 1.3.0.

### Added
- **ChatGPT / Codex shared by a host** (`codex-relay`, the Codex CLI's ChatGPT login): answers the API and Unity through
  the Responses API, streamed. Work stays Claude. Settings show it as "AI host, ChatGPT / Codex". Check R145 (extended).
- `bridge/host-relay.js` 1.0.0: the login relays (Claude → api.anthropic.com/v1/messages, Codex →
  chatgpt.com/backend-api/codex/responses, nothing else), token refresh through the provider's own CLI, usage headers.
- The host restarts itself on new code or config (within 20 s, between answers), so updates reach Gee's PC by themselves.
- `node bridge/handoff.js probe` (one tiny real call per shared login, no token printed), `service` (starts the boot-service
  installer: one administrator click at that PC), `restart-host`. Module: bridge/handoff.js 1.2.0.
- The AI Host download carries host-relay.js; a new host's config offers Claude and Codex relays (bridge/downloads.js 1.1.1).
- Gee's PC: shares Claude (fable-5.1, opus-5.5, sonnet-5.5, haiku-5.5; 90 % caps) and ChatGPT / Codex (gpt-6.1-sol) with
  mills, sponge and gee. Probed live: both answered "OK".

### Removed
- Remote Work runs on a host by default (still possible only with `allowWork: true` in that host's own config).
- The board: finished AI cards closed (T-45 … T-112); the Local / Cloud switch (T-109) is superseded.

## 2.2.1 — 2026-10-19

### Added
- `node bridge/handoff.js tools` builds the heart (`start-viberoller-ai-host.exe`), the knife (`stop-viberoller-ai-host.exe`)
  and the service program on a Windows host and puts "Start / Stop VibeRoller AI Host" on its Desktop, so a Work run can do
  it for the host's owner. Used on Gee's PC for the move. Module: bridge/handoff.js 1.1.0. (Project bumped by a MINOR
  change in a PATCH number because the move was already under way; noted here so it is not mistaken for a fix.)

## 2.2.0 — 2026-10-19

### Added
- **The hand-off: the move with nothing for Gee to do.** `node bridge/handoff.js pack --to ovh` makes the running hub pack
  its own data (accounts with their password hashes and device locks, chat + key, board, notes, pictures, saved sessions,
  settings; a tar.gz written by the hub itself), carry it to the server over ssh and run the restore there; meanwhile this
  hub pauses writes (`bridge/.maintenance`: sign-ins, chat, board and notes answer "VibeRoller has moved to …", the Work
  run doing the move keeps going). `node bridge/handoff.js switch` then hands off: `bridge/.handoff` makes `run.js`
  (start.bat, the autostart launcher, the service) run the **AI host** here instead of the hub, from the next graceful
  restart and at every start, with no tunnel. `undo` brings the hub back. A Work run may run it (allowed tool). Check R146.
  Modules: bridge/handoff.js 1.0.0 (new), bridge/run.js 1.1.0, bridge/server.js 2.2.0.
- Gee's PC's AI host config now offers his Claude to the group (mills, sponge, gee) besides Unity's engines, so once handed
  off his PC is the team's Claude with his login staying there.

### Changed
- **The portal is a normal address.** Sponge: "All of this is actually way too much for gee or mills to directly handle with
  this hyper security shit, I think instead of 404, people who go there with no username and password cant do anything."
  Both nginx site files drop the inside-only rule and the droids 404: anyone can reach the sign-in page at
  https://portal.unityailab.com, and the sign-in (username + password + device lock) is the protection. No SSH tunnel or
  proxy is needed any more. README, deploy/README and check R139 follow.

## 2.1.1 — 2026-10-19

### Fixed
- `deploy/setup-server.sh` sets `core.fileMode false` on the live checkout, so marking the deploy script executable on the
  server no longer counts as a local change that blocks the auto-deploy (it blocked the 2.1.0 deploy once; the server
  was fixed by hand). No module changed.

### Changed
- The portal is live: nginx sites installed, `portal.unityailab.com` has its DNS record and certificate, and the server
  hub's `publicUrl` is `https://portal.unityailab.com`.
- Both nginx site files now open `/node/` to everyone (an AI host connects from its home and answers only to its own id +
  secret) while everything else stays inside-only, and `viberoller.git.unityailab.com` follows the same rules as the
  portal, so there is no second, public sign-in page. (Needs the root reload once more.)

## 2.1.0 — 2026-10-19

**AI hosts become providers.** Nobody's login sits on the server: Gee's PC (or anyone's) runs the VibeRoller AI Host
with its own Claude login, the hub on the server hands it the Work runs and API calls of the people it allows, and relays
the answers back. The hub itself needs no Claude.

### Added
- **Host identity.** Every AI host has a UUID, a secret and a **nickname** the hub issues (adjective-animal, e.g.
  silly-panda; renameable). Settings → Connections → **My AI hosts**: New host, rename, remove, who may use each of its
  providers, and the AI Host download **with that host's config inside** (unzip, run; nothing to type). Registry:
  `bridge/ai-hosts.json` (gitignored). The legacy `aiNodes` config still works (name = id). Check R143.
- **The handshake.** The host says hello with what it offers (user, providers with models, caps and allowed users, Unity's
  engines, computer access) and both sides derive one session key (HKDF of the secret and two nonces; the key id `vr-…`
  shows in Settings). Work jobs and their events travel sealed with it (AES-256-GCM) on top of TLS.
- **Host providers.** What a host offers appears in everyone's AI pickers as `nickname · label (owner)`: its **Claude**
  (Work runs and API answers happen on the host, with its login), its **local models** (OpenAI-compatible endpoints such as
  Ollama, carried to the host and back, no port opened at home). Each provider names who may use it (the host's config,
  overridable by its owner in Settings) and its usage caps (`max5h`, `max7d`, default 90 %): the hub refuses a run past the
  cap until the window resets, and the host refuses too. The usage bars show the windows of whichever Claude you use.
- **Default = Gee's host.** Config `defaultProvider: "host:gee"` makes Gee's online Claude host the default for everyone
  who may use it; `hubClaude: false` tells a hub it has no Claude of its own. Settings → **AI source** picks host → provider
  → model per person (the Work dropdown shows the same, with offline / capped marked). Check R144, R145.
- **Remote Work runs.** Claude Code runs on the host in that machine's own worktree of the project (cloning VibeRoller or a
  sharedwork project from the hub's address when it has no copy: `hubRepoUrl`, `projectsGitBase`), the page says which
  host runs the reply, Stop stops it there, a follow-up resumes the same session there, and the PC / board / team tools of
  the run reach the hub through `/node/job/<id>/internal/<tool>` with a per-run token.
- **Direct computer access.** A host with `pc.folder` answers its owner's PC tools on that folder itself (the native app
  on your own PC), instead of the browser's open folder; the browser remains the fallback.
- **Streaming through a host.** A streamed answer (SSE, NDJSON: a local model, Unity's brain) is forwarded by the host piece
  by piece as its engine produces it (`POST /node/stream`), so words arrive as they are made instead of in one block at the
  end; whole answers (pictures, voice clips) still come in one go.
- The AI Host download now carries the MCP relays and the worktree tool (`common.js`, `worktree.js`, `pc-tools-mcp.js`,
  `board-mcp.js`, `team-mcp.js`); `ai-node.example.json` shows the full 2.0 config.

### Changed
- Modules: bridge/ai-nodes.js 2.0.0 and bridge/ai-node.js 2.0.0 (the protocol above), bridge/providers.js 1.2.0,
  bridge/usage.js 1.1.0 (per-source windows), bridge/server.js 2.1.0, bridge/openai.js 1.1.0 (default provider when the
  hub has no Claude), bridge/downloads.js 1.1.0, bridge/worktree.js 2.0.1 (`VIBEROLLER_HUB_REPO` / `hubRepo`),
  web/index.html 1.3.0 (AI source + My AI hosts panels, host-aware model dropdown). The Windows / Linux app is unchanged
  (Work and Settings → Connections are the site's); its shell list is unchanged.
- `/api/check` adds `effectiveProvider`, `defaultProvider`, `hubClaude`, `pcHost`; `/api/usage` adds `of` (the source).

## 2.0.0 — 2026-10-19

The hub's home is the lab server. This release makes the server layout real (the team's shared accounts, no root), turns
Work into a projects area, gives every computer a way to become an AI host from boot, and puts both applications behind a
Download button. MAJOR because one thing stops working the old way: Work's "shared folder" choice is gone (see Removed).

### Added
- **Projects (the sharedwork area).** Work's header now picks a **project** instead of "my worktree / shared folder". Every
  project is worked on in your own worktree of it (branch `work/<you>`), so people never overlap. **＋ New project…** starts
  an empty repo in the team's shared projects area (`projectsRoot`; `/srv/apps/sharedwork` on the server), **⤓ Pull in a
  repo…** clones an existing one by its https:// or ssh address (your saved Forgejo token signs you in; local paths and
  other git helpers are refused). Layout: `<projects root>/<project>/_repo` (main checkout, integration branch checked out)
  + one worktree per person. VibeRoller itself is the default project. Claude is told which project and worktree it is in,
  and merges with `worktree.js --project <name>` (commit, merge into the integration branch and push, sync); the hub's own
  tooling stays VibeRoller-only. `node bridge/worktree.js projects | new <name> | clone <url> [name]`, and every command
  takes `--project`. Checks R140, R141.
- **The private front door.** `deploy/nginx-portal.conf`: `portal.unityailab.com` (and `portal.git.unityailab.com`, which the
  lab's wildcard certificate already covers) shows the hub's sign-in only to connections that come from the server itself
  (an SSH tunnel used as a SOCKS proxy, a VPN that ends there); everyone else gets a real 404 page, *These are not the
  droids you are looking for.* The page the hub serves now always talks to the hub that served it, whatever name reached it.
- **VibeRollerAIHost, the boot-time service.** `bridge/host-tools/AiHostService.cs` is a Windows service that runs this PC's
  AI node from boot (before anyone signs in), restarts it, logs to `ai-node.log`; `install-ai-host-service.cmd` registers it
  (asks for administrator rights once) and lets signed-in users start and stop it; `build.cmd` builds it with the heart and
  the knife and offers the install. The heart (`start-viberoller-ai-host.exe`) starts the service, the knife
  (`stop-viberoller-ai-host.exe`) stops it; without the service they fall back to the pause switch as before. Linux:
  `viberoller-ai-host.service` + `install-ai-host-service.sh`, and the heart / knife scripts drive it. Check R136 (extended).
- **Download.** A Download button above Settings in the menu (and a download icon in the app's user panel) opens the two
  applications for Windows and Linux, built by the hub from its own code so they point back at this hub
  (`bridge/downloads.js` 1.0.0, a dependency-free zip writer): **Vibe Roller Desktop** (VibeRoller Chat in its own window:
  the browser's own install card, plus a launcher for Chrome / Edge / Brave with a Start-menu / application-menu entry) and
  **Vibe Roller AI Host** (the node, the service, the heart and the knife, a README with the steps, an example config that
  already names this hub). Signed-in people only. Check R142.
- **The deploy kit for the real server layout** (`deploy/README.md`): `setup-server.sh` now runs as the hub's account without
  root (user-local Node 22 + Claude Code, the live checkout, settings with a fresh node secret, the systemd user services,
  the nginx files in `deploy-ready/`, the printed root steps); `autodeploy.sh` on a user timer fast-forwards every push to
  `main` within a minute (syntax check, graceful restart), so no deploy key, runner or secret is needed; `pack-data … ovh`
  copies the archive over by scp; `restore-data.sh` runs as the account and shares the files with the group. Check R139.

### Changed
- `bridge/config.local.json` merges nested objects one level deep, so `assistant.via` can be set alone (bridge/common.js 1.2.0).
- `/api/check` answers `project` + `projects` (the list) instead of `workspace`; `/api/workspace` takes `{ project }` (an old
  `{ workspace }` still lands in VibeRoller); new `/api/projects` (GET list, POST new / pull in) and `/api/downloads`.
- Modules: bridge/server.js 2.0.0, bridge/worktree.js 2.0.0, bridge/common.js 1.2.0, bridge/downloads.js 1.0.0 (new),
  web/index.html 1.2.0, web/app.js 1.1.0, web/polish.css 1.0.2. The Windows / Linux app (VibeRoller Chat) gains the Download
  entry in its user panel; its shell list is unchanged (the page, app.js and polish.css load network-first).
- README: the address is the portal; the Work section describes projects; the host section describes the VibeRollerAIHost
  service. `.forgejo/workflows/deploy.yml` is the optional path now.

### Removed
- Work's **"shared folder"** choice (Claude working in the hub's own checkout). Everyone works in their own worktree of a
  project; a stored "shared" choice simply means VibeRoller. The old `start-viberoller-host.exe` / `stop-viberoller-host.exe`
  names are replaced by `start-viberoller-ai-host.exe` / `stop-viberoller-ai-host.exe` (the build removes the old ones).

## 1.5.1 — 2026-10-19

### Changed
- Work runs may run `ssh ovh …`: the host PC's SSH alias for the lab server (Gee's key, set up on that PC only), so
  the move to the server can be finished from a Work session instead of by hand. No module changed.

## 1.5.0 — 2026-10-18

### Added
- **The move kit is complete** (`deploy/README.md`): `deploy/setup-server.sh` does every root step on the server in
  one go (service user, live checkout, settings, systemd service, a deploy key locked to one command, the nginx site)
  and prints what is left for people; `deploy/pack-data.cmd` / `.sh` pack exactly the hub's data on the old host with
  the hub stopped; `deploy/restore-data.sh` puts it on the server, refusing anything that is not hub data and never
  overwriting existing accounts. Checks R139.
- **No AI host, everything still reachable.** When the machine lending Unity's engines is off or paused, the hub counts
  its node offline within a minute (`aiNodeOfflineMs`), refuses new engine calls at once instead of waiting, fails a
  job no node picks up within 40 s, and Unity says "my AI host is offline" plainly in her section and in the chat.
  Chat, the board, notes, Work's saved conversation, sign-in and people's own AI keys never wait on it. Checks R138.
- **Gee's PC after the move runs the AI node**: `bridge/start-node.bat` / `.sh`, `bridge/ai-node.example.json`; the
  autostart launcher and the heart start the node when `bridge/ai-node.json` exists (the hub otherwise); the node has
  a single-instance lock (`bridge/ai-node.lock`), so a second copy just closes.
- `node bridge/worktree.js repair [--repo <host folder>]`: after the host folder is renamed or moved (it became
  `VibeRoller` today), every person's worktree pointer is fixed; a Work run also heals its own worktree by itself.

### Fixed
- The GitHub Pages workflow ran on Forgejo too (and failed there); it now runs on GitHub only.
- Gee's autostart launcher pointed at the old `Mills` folder after the rename.

### Changed
- Modules: bridge/ai-node.js 1.1.0, bridge/ai-nodes.js 1.1.0, bridge/assistant.js 1.1.1, bridge/server.js 1.4.1,
  bridge/worktree.js 1.1.0. The deploy workflow now makes one SSH call to the key's fixed command.

## 1.4.0 — 2026-10-18

### Added
- **The new repo:** https://git.unityailab.com/GFourteen/VibeRoller (Forgejo), created through Forgejo's API
  and filled with main, develop and the work branches. Every release now goes to it as well as to the
  original Forgejo repo and GitHub (the go-live script pushes to every remote).
- **The server kit** (`deploy/`, hand-off in `deploy/README.md`): a Forgejo workflow that deploys every push to
  `main` (fast-forward the live checkout, check the scripts, graceful restart), off until the repo variable
  `VIBEROLLER_DEPLOY` is `on`; a locked-down systemd service (its own user, writes only under `/srv/viberoller`,
  no privileges); an nginx site (TLS, streaming unbuffered, WebSocket-ready, `/internal/` blocked, forged
  forwarding headers dropped); the server's own settings; the steps for the data move with Gee's approval.
- **Server mode:** `bridge/config.local.json` (gitignored) is laid over `bridge/config.json`; `publicUrl` gives
  the hub a fixed address (its page learns it, that origin may call the API), `behindProxy` treats nginx-forwarded
  requests as coming from outside.
- **AI nodes:** `bridge/ai-node.js` lends a machine's local engines (Unity's Ollama brain, image, voice, speech)
  to a hub running elsewhere, connecting out over HTTPS with its own name + secret; the hub (`bridge/ai-nodes.js`,
  `assistant.via = "node"`) sends only engine names and paths. The knife pauses a node. `/api/check` lists nodes.
  The throwaway test hub now runs every Unity check through a real node. Checks R137.

### Changed
- Modules: bridge/ai-node.js 1.0.0 (new), bridge/ai-nodes.js 1.0.0 (new), bridge/assistant.js 1.1.0,
  bridge/common.js 1.1.0, bridge/server.js 1.4.0.
- Hub law 10 now names the third remote (`viberoller`).

## 1.3.1 — 2026-10-18

### Fixed
- The version checks (R132, R133) counted Windows line endings as a change. `.bat`, `.cmd` and `.ps1`
  files are checked out with CRLF on purpose (`.gitattributes`), so the checks now compare files with line
  endings ignored. No module changed.

## 1.3.0 — 2026-10-18

### Added
- Autostart, so the host is always there for AI: `bridge/autostart-windows.cmd` puts a launcher into the
  Windows Startup folder that opens `start.bat` minimized at every sign-in (`remove` undoes it), and
  `bridge/viberoller.service` does the same on Linux as a systemd user service, at boot with lingering on.
  A second copy is harmless. Gee's PC already has the launcher.
- The host's AI switch and the host tools. `start-viberoller-host.exe` (black heart) and
  `stop-viberoller-host.exe` (knife) switch THIS machine's AI hosting on and off, never the system. Stopping
  pauses only the AI that runs here: Work runs on the host's Claude, the API's answers from it, Unity's
  assistant and Unity in the chat (who says she is paused). Chat, the board, calls, notes and people's own
  AI keys keep working for everyone. Starting brings it back at once, and starts VibeRoller here if it is
  not running. Build them once with `bridge/host-tools/build.cmd`, which draws the icons and compiles both
  tools with the C# compiler that ships with Windows, then puts shortcuts on the Desktop. Linux twins:
  `start-viberoller-host.sh` / `stop-viberoller-host.sh`. `/api/check` reports `aiHost`. Regression check
  R136. Modules: bridge/server.js 1.3.0, bridge/providers.js 1.1.0.

### Changed
- **Mills is now VibeRoller.** The new name shows everywhere people see it: the site, the sign-in card, the
  menu, Settings, the installed app (now "VibeRoller Chat"), the team-chat welcome lines, Claude's and
  Unity's instructions, the hub's messages and the README. The everyone-server takes the name once. Stored
  names stay as they were so nothing breaks: the `mills` account and server id, conversation ids, folders,
  `MILLS_*` settings and the `X-Mills-*` internal headers. Older sections of this changelog keep the old name.
- Modules: bridge/assistant.js 1.0.1, bridge/board-mcp.js 1.0.1, bridge/guild-perms.js 1.1.1,
  bridge/guilds.js 1.1.1, bridge/openai.js 1.0.1, bridge/pc-tools-mcp.js 1.0.1, bridge/run.js 1.0.1,
  bridge/server.js 1.2.1, bridge/team-mcp.js 1.0.1, bridge/team.js 1.0.1, web/app.js 1.0.1,
  web/board.js 1.0.1, web/calls.js 1.0.1, web/guild-settings.js 1.1.1, web/guilds.js 1.0.2,
  web/index.html 1.1.1, web/md.js 1.0.1, web/notes.js 1.0.1, web/polish.css 1.0.1, web/profile.js 1.0.1,
  web/sw.js 1.0.1 (app cache renamed, so installed apps refresh).
- Work runs may now also create Forgejo repos and inspect them (`fj repo create`, `fj repo view`,
  `fj whoami`), and manage git remotes, push, fetch and list remote branches (`git remote`, `git push`,
  `git fetch`, `git ls-remote`). Needed for the VibeRoller repo, because Forgejo does not create repos
  on push. No module changed.

## 1.2.0 — 2026-10-18

### Added
- Work keeps your place. The conversation on screen and your message queue, including a pending "Send now",
  are kept on the hub per person, in your own profile folder. They survive hub restarts, page reloads,
  closing the browser and switching devices.
- Opening Work brings back your previous conversation with its saved-session title, and Claude continues it.
  Queued messages come back too and start running. "New chat" clears the saved conversation.
- New module bridge/work-state.js 1.0.0 (routes /api/work/current and /api/work/queue, with size caps).
- Regression check R135: the conversation and the queue come back unchanged after a hub restart, and belong
  only to their person.

### Changed
- Modules: bridge/server.js 1.2.0, web/index.html 1.1.0.

## 1.1.0 — 2026-10-18

### Added
- The Mills server can have a named owner, shown to everyone, and the owner can name co-owners like on any
  other server. The hub's admin accounts keep owner rights there, so the host can always recover it. The
  Mills server can be handed on (Server Settings → Ownership) but never deleted.
- Host-set owners: `bridge/server-owners.json` (gitignored, so names stay off the remotes) gives any server
  its owner and co-owners when the hub starts. Each entry applies once, so later changes in Server Settings
  stick. Unknown accounts are skipped, and the audit log shows the change by "host".
- On this hub: Gee is the Mills server's owner, with Mills and Sponge as co-owners.
- Regression check R134.

### Changed
- Modules: bridge/guild-perms.js 1.1.0, bridge/guilds.js 1.1.0, bridge/server.js 1.1.0,
  web/guild-settings.js 1.1.0, web/guilds.js 1.0.1 (the OWNER tag shows on the Mills server too).
- The regression runner starts its throwaway hub with a pre-made server and a seed file (R113 ignores it).

## 1.0.1 — 2026-10-18

### Added
- Regression check R133: a change against production without a higher project version and its dated section
  at the top of this file fails the suite.

### Changed
- Hub law 15: every change of any tracked file (code, page, README, docs, tests, workflows) is logged here in
  the same commit, and moves the project version (PATCH at least). No module changed, so every module stays
  at its version.
- README: the Versions section names the changelog rule.
- Regression check R111 (the OpenAI-compatible API) failed once with "key none" and passed on the rerun; it now
  prints the hub's answer when making the key fails, so the next time shows the cause.

## 1.0.0 — 2026-10-18

The first versioned release: everything Mills does today, with every module at 1.0.0.

### Added
- Versioning itself: `versions.json` (project + 38 modules), this changelog, the version in Settings (site and
  the Windows / Linux app) and in `/api/check`, and a regression check (R132) that every changed module and the
  project move to a higher version with a changelog section.

### In this release (already shipped before versioning)
- Work: Claude Code in the browser with per-person worktrees, files, editor, past sessions, shared sessions,
  slash commands, usage bars, the OpenAI-compatible API with per-person keys, AI providers, My Forgejo.
- Chat: end-to-end encrypted, Discord-shaped: servers with built-in role levels (Owner, Co-owner, Super Admin,
  Admin, Super Moderator, Moderator, Member, @everyone), Discord's permission model with channel overwrites,
  invites (links, expiry, uses, temporary), password lock, moderation (timeout, mute, kick, ban), pins,
  slowmode, welcome / rules channels, coloured and mentionable / self-assignable roles, groups, private
  messages, calls with theater and screen share, YouTube embeds, markdown with Mermaid and KaTeX, Unity as a bot.
- Profiles (bio, pronouns, status, links, banner, local time, picture) apart from app settings.
- Board, Calendar, My day, Notes, Forgejo issue mirroring, the Unity assistant.
- Mills Chat, the installable app for Windows and Linux, with its own user panel.
- The look-and-feel layer (theme-driven, 58 custom icons) and themed scrollbars.
