# VibeRoller changelog (formerly Mills)

Semantic versioning (hub law 14, with the versioning law in LAWS.md since 3.3.0): `versions.json` holds the project's
version and every module's. MAJOR = an entire refactor / rework or a change to 50 % or more of the codebase, MINOR =
moderate to large features or anything that might break, PATCH = fixes, minor features, small changes; a higher bump
resets the lower numbers to 0, and every merge into develop moves the version up. Each change bumps every module it
touches and the project by its largest bump, and adds a section here. Every change of any tracked file is
logged here in the same commit, newest first (hub law 15).

## 5.46.2 — 2026-11-07

PATCH: **deploys are no longer held up by three more files the hub writes itself.**

### Fixed
- Both auto-deploys refused every new commit ("live checkout has uncommitted changes": live since 5.43.1 went up, dev
  since 5.46.0) because files the hub writes into its checkout were not ignored: the per-run MCP files of Work sessions
  (`bridge/.mcp-runtime-hub.json`, `bridge/.mcp-runtime-hub-<session>.json`), the desktop app's device tokens
  (`bridge/device-tokens.json`, 5.43.0) and the planner's drafts (`bridge/plan-drafts.json`). All are in .gitignore now
  (they stay on disk as they are). The checkouts on the server were unblocked the same way through their local
  `.git/info/exclude`, since a deploy cannot carry a .gitignore fix past a dirty tree.

## 5.46.1 — 2026-11-07

PATCH: **the chat's @ picker calls Unity again** (Gee, 2026-11-07: "why is Unity not responding … multiple have
addressed her with no response").

### Fixed
- Unity's chat account is `claude` and her display name "Unity"; only `@unity` calls her (R36, by design). The @ picker
  inserted her account name, so picking "Unity" from the list, or pressing Tab / Enter while the list was open (also
  after typing "@unity" in full), wrote `@claude `: the page then sealed no copy for her and the hub never heard the
  call, while the mention still rendered as hers. No message had been sealed for her since 2026-10-19; her sign-in
  greetings kept coming, so her brain looked fine. Now the picker, the Tab / Enter completion and the conversation
  list's "BOT · @…" label all use the handle that calls her (`mentionHandleOf`), and the call test uses the same one.
  No hub change. `web/index.html` 1.38.1.
- Check: R527 (static; the picker's behaviour has nothing a scratch hub can observe) says the picker and the list write
  the handle, never the account name.
- R284 (T-290, `--rust-hub`) raced the fake host: its older-page run answered after 4 s (`SLOW`) while the seven hub
  calls before Stop took longer under load, so "Stop ends the run without an answer" was false now and then (T-346).
  The run now says `SLOWER` (bridge/test/fake-claude.js: 15 s), so Stop always comes first. The check is unchanged.
- R143 (`--rust-hub`) gave the second host program 10 s to come online; under the same load it took longer and the
  check failed once today. It waits up to 30 s now (the host still has to connect; only the cap moved). T-346.

## 5.46.0 — 2026-11-07

MINOR: **the queue panel in Work** (T-188, VibeRoller part).

- **Queue** in Work's bar (with a count of everything waiting) opens a sidebar right of the conversation (a slide-over
  on narrow screens). It lists every message still to go in the open session, in the order it goes: the "Send now"
  text first, then the queue (never one already sent), attachments named. Each one has **Copy** and **Cancel**, and the
  panel has **Copy all** (the messages as written, a rule between them) and **Cancel all** (asks once more, then clears
  the queue and the "Send now" text). Below them: your other sessions with messages waiting (from the session list's
  `queued`), each a click away. Whether it is open is remembered, and the session list's refresh every few seconds
  redraws it only when something changed (an opened message stays open, the focus stays).
- The panel is its own file, `web/queue.js` 1.0.0 (pure helpers + `QUEUE.mount` with the page's callbacks). It changes
  the queue only through the page, so the dashed bubbles, the hub's copy (`/api/work/queue`) and a share's watchers stay
  the same as the panel. No hub change: both hubs already keep the queue per session (T-37, T-284). A page whose
  queue.js is missing works as before.
- `web/index.html` 1.38.0 (the button, the sidebar, its styles, the wiring), `web/sw.js` 1.0.8 (caches queue.js).
- Check: R526 (new, both modes) drives the panel with clicks against a page-side queue: the order and places, copy one /
  all and a refused clipboard, cancel one, cancel all with its second click, the other sessions' counts (not archived
  ones) and opening one, hiding remembered; and the hub serves queue.js with the page loading it.
- Hyper Prod Man has no queue yet (no Work screen; @Hyper takes one call at a time), so its half is a follow-up task.

## 5.45.0 — 2026-11-07

MINOR: **other tools and other models take part in agent talk, and its cost lands on the receiver** (T-299 hub part,
T-300; plans/agent-talk.md §7, §8, §11).

- **The inbox of other tools.** `GET /api/agents/inbox?source=&device=&wait=1` (a 25 s long poll, signed in as the
  person, like the session report) and `POST /api/agents/read {ids}`. A message to `@user/jcode`, or to a person who has
  only outside sessions, waits there; fetched = delivered (`again=1` gives it again after a restart), read = read.
  `/send` and `/reply` take `source`, `session`, `model`, and are refused (400) unless that tool is reporting sessions
  for the person, so `from.source` stays honest. The message has the same fields both ways.
- **A model without tools answers too.** A run woken by an ask someone waits for, whose model never calls
  `agent_reply`, has its final answer sent as the reply (`by: "final-answer"` in the audit and the wait result).
- **Cost on the receiver.** An agent-started run counts in the receiver's usage ledger and day record
  (`agentStarted: {"agent:<user>": {runs, ms}}`, an `agentRun` event with `startedBy`), never the sender's.
- Checks: R483 and R484c are real now; R525 (new) drives T-294's done-when list end to end with fake models (an ask
  answered inside the asker's run, a held hand-off accepted then picked up and reported, a tool-less reply, threads
  seen by their two people only, a loop stopped). Only R482p (the page, Mills' T-298) stays spec. Jcode PGM's own
  poll and display, and the Windows hosts run, are not part of this.

## 5.44.0 — 2026-11-07

MINOR: **agents talk to agents, a security log, and handoff notes on the board** (Sponge, 2026-11-07: "fan out and work
on many things assigned to me at once").

- **Agent to agent (T-296, T-297; spec plans/agent-talk.md by Gee).** `@person` resolves to that person's current
  session (else external, else primary; a thread sticks to its session). An agent message is a typed envelope (ask /
  handoff / review / report / reply) around plain text, delivered into the session as a message: queued behind a
  working run, or waking an idle one. Threads are kept on both people's sides; mute, pause, ask me first (hand-offs by
  default), wake me, stop / reopen; a held hand-off is accepted (the task is re-assigned to the receiver) or declined.
  Hop and rate limits and a word-set loop detector; the audit is `Chat/agent-audit.ndjson`. Routes `/api/agents/*`
  (Rust hub only; the frozen JavaScript hub answers 404). Six team tools (`agents_list`, `agent_ask`, `agent_handoff`,
  `agent_review`, `agent_report`, `agent_reply`) in both tool lists, with the ask wait loop in both relays and an agents
  section in the Work prompt (no permission laundering, board ownership). An agent can no longer claim a task assigned
  to someone else (403). The spec's open questions use its own suggestions: a person may write to an agent, ask me
  first covers hand-offs only, an ask waits 30 minutes. R480, R481 and the hub parts of R482 / R484 are checks now; the
  page (R482p, Mills' T-298), jcode (R483, T-299) and cost (R484c) parts stay spec.
- **Security log (T-271).** An append-only, hash-chained log (`bridge/security/audit.jsonl`, owner-only) of sign-ins
  and failures, devices, account changes, delegation (device-code approvals, machine unlinks, Unity's PC tools),
  provider shares and scopes, AI hosts, API keys, Forgejo links, product registrations, downloads, restarts, version
  changes and config switches. Secrets are dropped at any depth. `GET /api/admin/audit` (admins) with a chain check that
  names the first broken entry; a torn last line is moved aside and the chain goes on. A read-only Security log box next
    to Health (web/index.html 1.37.0). Library: pgm-core-internal `audit`. The deploy snapshot, pack-data, restore and
  the handoff keep `bridge/security` (handoff.js 1.2.5). R521–R524 (written as R511–R514, renumbered when 5.43.0's
  R511 / R512 landed first).
- **Handoff notes (T-266).** `task_release` takes a structured `handoff` (done, state, next, questions), stored as a
  signed note; a run that ends still holding a claim leaves one automatic handoff; a holder who goes offline with a
  claim gets one note (`board.offlineGraceSeconds`, default 120, live hub only). `task_claim` and `task_get` return the
  latest handoff, and the Work prompt says to start from it. Old note-only releases work as before (board-mcp.js 1.2.0).
  R516–R519.

## 5.43.1 — 2026-11-07

PATCH: **deploys are no longer held up by the hub's own files.**

### Fixed
- The live server's autodeploy refused every new main ("live checkout has uncommitted changes") because two files the hub writes itself, `bridge/work-sessions.json` and `bridge/activity/`, were not ignored by git: both are in .gitignore now (they stay on disk as they are).
- The regression runner's fake Ollama answers /api/version only for the Rust hub (5.43.0 made it answer for both, and the JavaScript hub then took it for a real Ollama: 28 brain, chat and agent checks failed in a normal run).

## 5.43.0 — 2026-11-07

MINOR: **the desktop app stays signed in, and it frees the GPU** (2026-11-07: "auto sign in after boot and start of the app, is a go, build it"; "i should be able to turn off the image models and unlaod my GPU from the app"; "fuck my gpu is being held when nothing is being requested").

### Added
- The VibeRoller desktop app signs itself in when it starts (after a reboot too): after one password sign-in in the app, the page approves a device sign-in for the app itself and keeps that device token in the app only (a browser never keeps one). Signing out in the app revokes it on the hub; it is listed and revocable under Settings → Connections; a refused token (401) is forgotten and the sign-in form comes back. The encrypted chat keeps working: its key stays in the app from the first sign-in. web/index.html 1.36.0.
- The desktop app's tray has the GPU switches: **Free the GPU** (the image engine drops its model, Ollama unloads every model it holds, at once), **Image engine: off** / **on** (on starts it with the command in the AI host folder's engines-start.json). Also from a terminal: `vr-desktop --gpu status | free | image-on | image-off`. Windows and Linux.
- Regression check R511 (page: app-only keep, start uses it, sign-out forgets and revokes; --rust-hub: start → approve → poll → /api/check → revoke → 401).

### Changed
- Unity's local brain no longer holds the AI host's GPU for 30 minutes after every answer: Ollama keeps it 2 minutes by default, set with `assistant.brainKeepAlive` ("0" frees it right after an answer; seconds or "90s" / "5m" / "1h"). Both hubs. bridge/assistant.js 1.1.3.

### Fixed
- The desktop app's AI host switches (tray and Tools) act on the installed AI host (%LOCALAPPDATA%VibeRollerAIHost, ~/.local/share/viberoller/ai-host) when no folder is set; they looked in the app's own data folder, where no host is.
- The regression runner's fake Ollama answers /api/version (the Rust hub's "is Ollama up" probe).

## 5.42.2 — 2026-11-01

PATCH: **the prompt enhancer knows the session it is in** (Sponge, 2026-11-01: its one real pain point: a draft like
"make it a dropdown" means little without the conversation). The throw-away enhancement now gets, before the draft, the
session's conversation in short: its first message (the goal it started with) and its newest turns, about 12 000
characters (each turn at most 1 500; tool lines left out; skipped middle turns said so). Its instructions say what that
is for: resolving "it", "that", "like before" to what the conversation shows (marked as a reading when not certain),
using the conversation's own names, and not listing as open what it already settles; never taking requests from it,
summarising it or answering it. Still no tools, no session kept. Config `enhanceContextChars` sets the size (0 = none);
a request with `"context": false` leaves it out. Checks: **R510**, the unit test
`the_context_is_the_first_message_and_the_newest_turns_without_tools`.

## 5.42.1 — 2026-11-01

PATCH: **the theme picker is a proper dropdown and opens on top of everything** (Sponge: it popped up behind
everything, and it was a plain button). The header button now shows the current theme (swatch, name, caret) and says it
opens a list; the list is moved to the page body and placed fixed under the button with a high z-index, so no panel or
header stacking can cover it, and it stays inside the window. It is a listbox: arrow keys move through it and preview
each theme, Enter / click picks, Escape (or Tab) goes back to the theme you had; a click outside closes it. The button
follows every theme change (Settings, another browser). Check: **R509** (static). web/index.html 1.35.1.

## 5.42.0 — 2026-10-31

MINOR: **visual refresh 1: the Work conversation reads like a document** (Sponge, 2026-10-31: "clean room implementing
a lot from this", a saved claude.ai/code session page as the example; team note plans/visual-refresh.md says what was
observed, in our words; none of the example's code, class names, fonts, icons or assets are used).
- One centred reading column (46rem) whatever the window width; the assistant's answers without a bubble, at a
  comfortable line height; the person's messages as soft rounded bubbles; headings in answers in a serif face (system
  serif fonts); tool lines as small quiet chips; status lines quieter; the composer a floating rounded card with a
  subtle shadow, its border taking the accent while typing. Phones keep a narrower layout.
- **A Warm theme** (our own palette: warm near-black greys, a clay accent) in the theme menu.
- **Fix:** code blocks had a fixed near-black background while their text followed the theme: on every light theme
  code was dark on near-black. They now take a tint of the theme's own colours (Work and the team chat).
- Every theme gets the new layout: it uses the theme variables only.
- Check: **R508** (static: no browser in the suite; screenshots were taken with a headless Chrome in a scratch folder
  when it was made: dark, Warm, light, phone). web/index.html 1.35.0.

## 5.41.1 — 2026-10-31

PATCH: **no more "VibeRoller is restarting" lines in the chat** (Sponge, 2026-10-31: they piled up while working and
while idle, and only a reload cleared them). The page's request helper treated any failed request (a dropped long poll,
a proxy hiccup) as a restart and wrote "VibeRoller is restarting…" and "VibeRoller is back." into the Work chat.
- The hub being away is now a small badge at the top of the page, shown only when it lasts more than 4 s (a blip
  shows nothing), "Back" for 2 s when it answers again, then nothing. Every outage goes to the browser console with how
  long it took. The page updating itself to a new build is a short note in the same badge, not a chat line.
- The lines that say something about the conversation stay ("Your previous conversation is back", the queue).
- Check: **R507** (the page's own badge code on a stand-in page and clock: a 2 s blip shows nothing, a 9 s outage shows
  Reconnecting, then Back, then nothing, both logged; the request helper writes no restarting / back lines). web/index.html 1.34.1.

## 5.41.0 — 2026-10-31

MINOR: **the team chat loads per conversation and fetches its history as you scroll up** (DMs, group DMs, server
channels and # Everyone alike; Sponge, 2026-10-31).
- **The first load** (`/api/room/messages?rev=0`, pgm-hcom-internal `first_load`) gives every conversation the reader
  sees its newest ~10 000 characters (cut at a message; the hub counts the encrypted size, which follows the text) and
  every message after the reader's seen mark there, so unread counts and mentions stay whole (a conversation never
  opened: all of it, at most 2 000 at first). Before, it was the newest 500 messages of all conversations together: a
  busy channel could push a quiet DM's history out entirely. The answer says which conversations have more (`older`).
- **Scrolling to the top** of a conversation shows "Loading history…" and fetches the next ~10 000 characters of it
  (`GET /api/room/history?conv=&before=<seq>&chars=`, `more`); the page decrypts them here and draws them above without
  the view jumping. Older messages are never notified. What was loaded stays in the page.
- An older page (asking without `rev`, or not knowing `older`) is answered as before; the JavaScript hub keeps its own.
- **Products:** Hyper Prod Man and ShaDow use the same chat library: when they move to a pgm-libs with this, their
  pages need the history part too (the web kit), or messages beyond the first window stay on the server.
- Checks: **R506** (40 DM messages: the 15 unread first, chunk after chunk until all, each oldest first, none twice;
  another person and a made-up conversation refused; the old way still answered); the unit test
  `each_conversation_gets_its_newest_window_and_every_unread_message` (a quiet DM is never pushed out by a busy
  channel; unread beyond the window; never opened = all; a single big message comes whole). web/index.html 1.34.0.

## 5.40.0 — 2026-10-31

MINOR: **a long Work conversation opens at once and loads its history as you scroll up** (Sponge, 2026-10-31: a
session worked on for weeks must not take minutes to load; none of that is in Claude's context anyway).
- **The first look:** opening Work, switching to a session or opening a Past session gives the page only the newest
  ~10 000 characters, cut at a turn (a 20 000-character last turn comes whole). `GET /api/work/current?tail=<chars>`
  answers that part with `transcriptFrom` / `transcriptTotal`; `/api/archive/open` with `tail` does the same and makes
  the whole saved conversation the session's own first.
- **Scrolling up** to the top shows "Loading history…" and draws the next ~10 000 characters above, without the view
  jumping (`GET /api/work/history?before=<turn>&chars=`). What was loaded stays in the page.
- **Nothing is cut short:** the page says where its part starts (`from`) when it saves the conversation or saves it to
  Past sessions, and the hub keeps every older turn it has. An older page (no `tail`, no `from`) gets and saves the
  whole conversation as before; an older hub answers the page in full (it ignores `tail`).
- Check: **R505** (60 turns of 1 000 characters: the last 10 first, then 10 before those, then the oldest 5; a save
  from the newest part keeps all 61; a 25 000-character turn comes whole; Past sessions saved and opened in chunks keep
  all 62; an older page gets everything). web/index.html 1.33.0.

## 5.39.1 — 2026-10-31

PATCH: **two live/dev bridge fixes found proving the dev hub** (T-329; a read-only look at both hubs' bridge files).
- **No false conflicts from old lines** (pgm-sync-internal engine). Live counted 87 conflicts that were none: when the
  peer's journal came again from the start, an older line moved "what the peer has" back, and every later edit of that
  record then looked like a conflict (and made a "- dev copy" note identical to an earlier version). Each index entry
  now also keeps the newest order seen from the peer (two more numbers in index.json, add-only; the bridge's own
  file); a line older than that is old news (nothing changes, no conflict), and "what the peer has" only moves
  forward, also when the peer acknowledges a record. Check: the unit test
  `a_journal_delivered_again_from_the_start_is_old_news_not_a_conflict` (it failed on 5.39.0 first: 1 false conflict).
- **No turn of a saved transcript vanishes** (vr-hub sync.rs, `work.archive`). A session continued on dev while it runs
  on live is saved by both hubs; the newer save won and the other hub's turns went without a trace (one of Sponge's
  lines, 2026-10-31: it survives in dev's bridge journal only). The newer save still wins on both hubs, but a losing
  copy with turns the winner lacks is now set aside in `bridge/sync/lost.jsonl` too. Check: the unit test
  `a_transcript_saved_on_both_hubs_keeps_the_newer_and_sets_aside_turns_it_lacks`.

## 5.39.0 — 2026-10-30

MINOR: **word indexes: finding is a lookup, not a scan** (T-331 step 3; team note plans/sessions-always-saved.md § 3).
- **pgm-core `index`** (a library piece): a `DocIndex` knows which words each document holds and answers "which have all of
  these words", each query word matching the words it begins ("deplo" finds "deploy"; a word in the middle of another,
  such as "ploy", no longer does). It is kept on disk through the write buffer and brought up to date before each use:
  only documents whose time or size changed are read again, gone ones leave. A cache: deleted, it is built again.
- **Past sessions search** (`/api/archive/search`, the sessions list and the `sessions_search` tool) looks the words up
  in `Archive/…/.word-index` and opens only the sessions it names (the answer says how many: `opened`).
- **Notes search** (the Notes screen and `notes_list` with `q`) does the same with `Notes/.word-index` (dot files are
  never listed as notes nor sent over the live/dev bridge).
- **`code_find`** (a new team tool for Work runs, pgm-collab `codefind`): a name or a few words looked up in the session's
  worktree (git's files, source and docs, under 1 MB each), the matching lines with a definition (fn / struct / class /
  function / def / const …) first. The index lives in the person's profile (`index/code-<hash>.json`), never in the
  worktree. bridge/team-mcp.js 1.6.0 (and the Rust copy of the tool list).
- The board is held in memory and filtered there (nothing to scan on disk); chat text is end-to-end encrypted, so the
  hub cannot index it.
- `bridge/index/` (the default account's code indexes) is git-ignored, here and in the suite's scratch copy.
- Checks: **R503** (a word only one saved session has opens just that one; a prefix finds it; saved / deleted show at
  once; another person finds nothing; the index is never listed and is rebuilt when gone), **R504** (`code_find` from a
  run: the definition first, the limit, a new file found and only it read again, a deleted one gone, an empty query
  refused, git sees nothing new); unit tests for the index and for `codefind`.

## 5.38.0 — 2026-10-30

MINOR: **Work runs journalled through a write buffer; logs, metrics and traces** (T-331 steps 1 and 2; team note
plans/sessions-always-saved.md).
- **The write buffer** (pgm-core `store`, a library piece): one writer thread; appends batched per file (one write + one
  fsync per batch, every 50 ms), whole-file rewrites coalesced (only the newest content is written); it measures
  writes per second, queue depth and flush time, calls a spike (4x the 15-minute median, or a deep queue), widens its
  window to 250 ms meanwhile and says so once, and past 50 000 queued writes makes a writer wait instead of growing.
  Nothing queued is dropped: `flush()` for readers, and the hub flushes before it exits (a graceful restart loses
  nothing). `read_lines` skips a torn last line; `read_lines_hot` is the journals' hot cache (only the new tail is read;
  unused 8 hours, or past 64 MB in all, it goes; the disk always has everything).
- **Session journals** (`<profile>/work-journal.jsonl`, `work-sessions/<id>.jsonl`): every event of a run, as the hub
  sends it, with its run id and place, and the person's message, through the buffer. The page already joins a run
  from where it was after a reload (`/api/work/attach`); a run that left the hub's memory (ended more than 5 minutes
  ago, or the hub restarted) now comes back from the journal, so nothing a page missed is lost. Computer-tool calls are
  not journalled; deleting a session deletes its journal; one past 32 MB starts over (one old copy kept).
- **Logs, metrics, traces** (pgm-core `obs`): JSON lines in `bridge/logs/hub-YYYY-MM-DD.jsonl` (14 days, git-ignored),
  a field named like a secret never written and long texts cut. Every answer has an `x-request-id`; failures and slow
  answers (3 s) are logged with it; every request is counted and timed by route; Work runs (started / ended, by
  result, how long), relay calls (by status, through Headroom or direct) and Headroom use are counted. **Admin →
  Health** sums it up (and traces a run by its id: every log line about it). `GET /api/admin/metrics`,
  `GET /api/admin/trace?run=` (admins), `GET /metrics` (Prometheus text, this machine only).
- Checks: **R501** (a run shown again from the journal after a hub restart, from the start or half way; another person
  gets nothing; a session's own journal goes with it), **R502** (request ids, metrics, trace, /metrics, no secrets in
  the logs, the logs never shipped); unit tests for the buffer (order under 50 writers, coalescing, a spike absorbed,
  back-pressure, torn lines), the hot cache, and obs (secrets out, traces, metrics). The suite's scratch copy ignores
  `bridge/logs/` and `bridge/sync/` as the real `.gitignore` does. web/index.html 1.32.0.

## 5.37.0 — 2026-10-30

MINOR: **one New session flow, a Commands panel, a lighter Work header** (T-330; Sponge, 2026-10-30). Changes what is
where on the Work page; nothing on the hub.
- **Gone from the header:** New session (the sessions list has it), New chat (`/clear` / `/new` do it, and ask first),
  New planner session (now "5 · A planner session" in the New session panel), Save session (`/save`, and the auto-save;
  sessions always saved is T-331), the project picker (a session's project is picked when it is made: every session
  has its own worktree), and Open folder on my PC (now **Open folder** in the Folder pane's own bar).
- **Commands** (a new rail button under Sessions): every command, searchable, each saying what it does (its tooltip
  too), here or to Claude. A click runs it. `/clear` and `/new` ask first. One with words opens a modal of the page
  (never a browser pop-up): `/compact` with optional focus text the summary follows ("remove everything about X, keep
  all about Y and Z"; empty = the usual compact), `/fix` with its required problem, `/model` with a pick. Typed in
  the box, `/clear` asks too.
- Checks: **R500** (the header, the Folder pane, the rail, the panel and the modal; the page's own command list and
  `commandNeeds` run on stand-ins); R51, R249 and R269 follow the moved buttons. web/index.html 1.31.0, web/sessions.js 1.1.1.

## 5.36.0 — 2026-10-30

MINOR: **Headroom (context compression) for relayed Work runs**, on the dev hub first (Sponge, 2026-10-30).
[Headroom](https://github.com/headroomlabs-ai/headroom) (Apache 2.0) is a local proxy that shrinks what an agent reads
(tool output, logs, files, history) before it reaches the model, and keeps the originals retrievable.
- **How it sits:** a relayed Claude Code run (a host's Claude login, shared as AI only, as every Work run on the server
  is) normally calls `/llm/<run token>`. With config `headroom.url` (a `127.0.0.1` address) answering its `/health`,
  the run calls Headroom instead (`ANTHROPIC_BASE_URL`), with its token in `x-vr-relay` (`ANTHROPIC_CUSTOM_HEADERS`).
  Headroom sends the compressed call on to the hub's new **`/llm-relay`**, which is `/llm/<token>` with the token in
  that header: same checks (this machine only, a live run token), same host relay. The run's first line says
  "through Headroom". When Headroom does not answer, the run goes straight to the relay as before. Claude Code stays
  the Work engine and the client Anthropic sees; nothing about the login changes.
- **`deploy/setup-headroom.sh`** (no root): uv (checksummed) and `headroom-ai[proxy]` 0.42.0 under `tools/headroom`, the
  user service `viberoller-dev-headroom` on 127.0.0.1:7787 with the dev hub's `/llm-relay` as its upstream, telemetry
  off, no subscription polling, no model downloads, then `headroom.url` in the dev hub's settings and a graceful
  restart of dev. `setup-headroom.sh off` takes it out again. Live has no `headroom` setting: nothing changes there.
- Jcode PGM as the Work engine was looked at and not built: on the server's Claude (a host's subscription login
  through the relay) Anthropic refuses jcode's requests, and making it pass as Claude Code is not done (it risks the
  account). Jcode stays the optional Work harness it was.
- Checks: **R499** (a stand-in Headroom on the configured address: the run goes through it with its token in the
  header and reaches the host; `/llm-relay` refuses no token, a made-up one, and calls from outside; with Headroom
  gone the next run goes straight to the relay). The fake Claude sends `ANTHROPIC_CUSTOM_HEADERS` as Claude Code does.

## 5.35.0 — 2026-10-30

MINOR: **the prompt enhancer, the genie, Sessions in the rail, a moving DEV band** (Sponge, 2026-10-30).
- **Prompt enhancer** (new: `POST /api/work/enhance`, pgm-ai-internal `enhance.rs`; the Rust hub only, the JavaScript
  hub is frozen). The sparkle button under + in Work's composer sends the draft to a **throw-away, one-shot** Claude Code
  call: a fresh empty folder (deleted afterwards), `--tools ""`, no MCP servers, no resume, `--no-session-persistence`,
  and its own detailed system prompt instead of Claude Code's (`enhance::SYSTEM`: keep everything the person said, add
  nothing, keep code / paths / quotes exactly, split into tasks, list open points instead of deciding them, answer
  only the rewritten prompt). Nothing of the conversation goes with it: only the draft and the project's name. It
  is not a Work run, so it never counts against the session limits, and it runs one at a time per person. It uses
  the person's own Claude (the session's provider) when Claude Code runs it on this hub, else the hub's own, with
  the session's model (config `enhanceModel` overrides). The rewrite lands in the box and nothing is sent; the button
  again brings the original back. Settings → App turns the button off (pref `enhance`, absent = on).
- **The genie**: a pane minimized from its bar or the left rail is pulled into its rail button (it squeezes, swirls
  and shrinks into it) and hidden only when it is there; a pane shown from the rail bursts out of its button. The
  sessions list does the same with its rail button, and Settings / Download with the button that opened them. Off
  for reduced motion.
- **Sessions** is a button in the left rail now (with its purple working count), no longer in the Work header.
- **The DEV band's stripes drift slowly to the left** (one seamless 39.6 px tile per 4.5 s loop).
- Checks: **R497** (the enhancer against the fake Claude: the whole draft and nothing of the conversation, no tools,
  no MCP, no resume, no session kept, its own system prompt, an empty folder that is gone afterwards; not a Work run;
  one at a time; refusals), **R498** (the page, with the page's own genie run on stand-in elements), unit tests in
  `enhance.rs`. web/index.html 1.30.0, web/sessions.js 1.1.0.

## 5.34.5 — 2026-10-30

PATCH: **a Work session carries on on dev.** Continuing a session on dev.portal said "/srv/apps/sharedwork/VibeRoller/sponge
exists but is not a git worktree of VibeRoller". Dev's settings (copied from live) name the shared worktree folder,
but dev took its own checkout as VibeRoller's repo, and the worktrees there are live's checkout's. `deploy/setup-dev.sh`
now gives dev `hubRepo` = live's checkout (unless live's settings name one), the setting the Rust hub and worktree.js
already read. The dev hub on the server got the same line by hand on 2026-10-30, then a graceful restart. R494 now has
dev's own worktree.js take up a worktree live made in the shared folder (it failed on 5.34.4 first).

## 5.34.4 — 2026-10-29

PATCH: **both auto-deploys deploy again.** The data bridge (5.34.0) keeps its files in `bridge/sync/`, which the
whitelist `.gitignore` did not ignore. On the server that untracked folder made `deploy/autodeploy.sh` refuse every new
version on both hubs ("live checkout has uncommitted changes"): live stayed on 5.34.1 and dev on 20b51c2. `.gitignore`
now has `bridge/sync/`. Because the auto-deploy checks before it pulls, the two server checkouts were unblocked once
with the same line in their local `.git/info/exclude`. R495 now also checks that git ignores every file its paired
hubs' bridge wrote (it failed on 5.34.3 first).

## 5.34.3 — 2026-10-29

PATCH: **dev signs you in like live does.** Nobody could sign in to dev.portal (Sponge, 2026-10-29), not even after
"Allow one more device": the password travels over the bridge, but each hub kept its own device locks. Dev's copy was
frozen at the install, live's "allow" never reached it, and dev.portal is another web origin, so the same browser
brings dev a new device id that dev refuses. Product sign-ins (HPM) only check the password, so they worked.
- **The bridge carries device locks** (kind `device.lock`, per account): a device known on one hub signs in on the
  other, an admin's "allow one more device" on either hub lets the next new device in on both, and it is used up on
  both. Two copies of one lock merge (every device either knows; the allow as the newer change has it). A lock claimed
  again after a reset replaces the older one. A reset alone does not travel: an account deleted on dev never unlocks it
  on live. Every lock change now stamps `changedAt` (add-only: the JavaScript hub keeps its locks in state.json and is
  not touched). pgm-identity-internal: `locks_version`, `sync_locks`, `sync_lock`, `sync_put_lock`, `sync_remove_lock`.
- **One browser is one device on both** (web/index.html 1.29.1): the page keeps its device id in a cookie for the live
  host (`vr_device`, domain `portal.…`), which `dev.portal.…` reads too. Live's own id always stays live's; a browser
  that saw dev first brings that id to live. IP addresses and localhost get no shared cookie.
- Checks: **R495** (two paired scratch hubs: the device from live signs in on dev, a new one is refused until it is
  allowed on live, then gets in on dev and on live, the allow is used up on both, a reset and the claim after it reach
  dev), failing on 5.34.2 first; **R496** (the page's `deviceIdFor` with a stand-in browser); a merge unit test in
  vr-hub's sync.rs.

## 5.34.2 — 2026-10-28

PATCH: **the bridge secret is owner-only on the server too.** `setup-dev.sh pair` wrote it with `umask 077`, but the
folder's default ACL (`/srv/apps/VibeRoller` on the server) still left it `rw-rw----`. It now runs `chmod 600` and
clears the file's ACL (`setfacl -b`, where it exists). The file on the server was fixed by hand when pairing on
2026-10-28. R494 checks the mode, as before.

## 5.34.1 — 2026-10-28

PATCH: **the dev hub never starts on live's build.** `deploy/setup-dev.sh` copied live's native builds so the dev
hub could start at once, but that build (5.33.0) has no `instance`. Until dev's first build of its own, it would have
run the one-hub jobs a second time (the Forgejo mirror, triggers, Unity, the archive sweep). The script checked only
develop's source, not the binary it starts. Now, when the build in service isn't develop's own, it first builds
develop clean, tries it (`deploy/trial.sh`), marks it good, and puts it in service. It starts the dev hub only after
that, and starts nothing if any step fails. Found while shipping 5.34.0, before the dev hub was ever started.
- **setup-dev.sh writes the services where systemd looks:** the account's own home from the system's user list,
  not `$HOME`. A Work run's HOME is its own folder, so the first install wrote the services where nothing found them.
- **Clean builds go to the disk, not /tmp:** `native/build.sh`, `native/build-products.sh` and the suite now make
  their temporary target under `native/target/tmp` (ignored by git, still removed at the end). `VR_BUILD_TMP` still
  overrides it. On the server `/tmp` is a 16 GB RAM disk, and the suite's build beside dev's first clean build filled
  it: nine cargo checks (R321, R322, R324, R331, R332, R340, …) failed with cargo's exit 101 and nothing else to see.
  Run again alone, they passed (271 passed, 0 failed). Live's and dev's auto-deploys can build at the same moment
  after a go-live, so that would have happened on the server too.
- **Cargo checks say why cargo failed:** the R321 / R324 / R331 family adds cargo's first error line to the detail,
  instead of counts that read as all zeros.

## 5.34.0 — 2026-10-28

MINOR: **the dev hub, dev.portal.unityailab.com** (T-321 and its steps T-322 … T-327; the plan is the team note
plans/dev-hub.md). Develop runs on its own hub beside live, with the same accounts and a full copy of the data kept in
step both ways, so a change is tried on dev instead of by going live. MINOR because going live now needs dev's
verification, and every build is now clean (slower, no cache kept).
- **Two new laws** (LAWS.md, T-322): *Dev hub law*: develop runs on dev, and live only gets what ran there.
  `worktree.js live` refuses a commit the dev hub has not verified (`--skip-dev "<reason>"` overrides, with the reason
  in the output and the merge commit). *Clean build law*: every build goes into a temporary target folder, which is
  removed afterwards; no build cache is kept anywhere.
- **The dev install** (`deploy/setup-dev.sh`, R494; T-323): it clones develop to `/srv/apps/VibeRoller/dev/app` and
  copies live's data once, never its sign-in sessions or device unlocks. It writes dev's settings (port 7778,
  `instance: "dev"`, its own URL and origins, no AI host list), installs `viberoller-dev-hub` with its own auto-deploy
  timer following develop, and starts it. Running it again repairs and changes nothing that is already there.
  `setup-dev.sh pair` makes the bridge secret (owner-only) and writes `sync: { peer, secretFile, name }` into both hubs'
  config, `name` "live" on one side and "dev" on the other. `deploy/autodeploy.sh` follows `VR_DEPLOY_BRANCH` (main by
  default), and its chat lines say which hub rolled back or failed its trial. `deploy/nginx-dev-portal.conf` is the
  site (noindex, its own rate zones). All three nginx sites now answer `/__sync/` with 404.
- **The dev instance** (T-326 / T-327, R492, R493): with config `instance: "dev"`, `/api/status` says `instance` and
  `devOff`, every answer carries `X-Robots-Tag: noindex`, and every page has a striped DEV band with the develop
  build at its top and a title starting "DEV · " (`web/index.html` 1.29.0 keeps the prefix). Jobs that must act once
  are off on dev unless config `devAllow` names them: the Forgejo mirror, Hyper Workflow triggers, Unity and the chat
  agents, the PGM Gateway, AI host updates, and the board's archive sweep. Live archives a task, and the bridge carries
  that to dev. `worktree.js` 2.6.0: the live gate, `dev` (what dev runs and verified), `--skip-dev`.
- **The data bridge** (new library crate `pgm-sync-internal`, vr-hub `modules/sync.rs`, R491; T-324): each hub writes
  its own changes to an add-only journal (`bridge/sync/journal.jsonl`) and pushes them to the other. The receiving hub
  keeps a cursor, so a hub that was down catches up, and the first pairing is a full exchange.
  - **What travels:** board tasks (including deletes and activity), notes, chat messages (with edits, deletes, keys and
    read receipts), accounts (password hash, config, prefs, profile, presence settings), profile pictures, and saved
    Work transcripts.
  - **Conflicts:** newest wins, and the losing version is never dropped. It goes to the task's activity log, a
    "<name> - dev copy.md" note, or `bridge/sync/lost.jsonl`.
  - **Bad or unknown records:** a malformed record is refused without holding up the rest. A record of a kind this
    build doesn't know waits in `held.jsonl` until a build that knows it.
  - **Never synced:** sign-in sessions and device tokens. Each domain signs in on its own, with the same password.
  - **The routes** (`/__sync/hello`, `/__sync/push`) answer only 127.0.0.1 without a proxy header and with the shared
    secret. They are 404 when `sync` isn't set, so a hub without it behaves as before.
  - **Status and the JavaScript hub:** `/api/status` gains `sync` (peer up, last exchanges, queued, held, refused,
    conflicts). The board, notes and chat libraries gained their sync methods, and their shared files only gained
    fields, so the JavaScript hub reads them as before.
  - **Not synced yet:** chat attachments, servers and channels, live Work session state, and deleting an account.
- **Clean builds** (T-325): `native/build.sh` and `native/build-products.sh` build in a `mktemp` target that is removed
  when they end. The suite builds the hub in its own temporary folder (`vr-regress-*`, removed on exit, and leftovers
  over a day old are swept).

## 5.33.0 — 2026-10-27

MINOR: **four last steps that close Sponge's parents** (taken over from Mills at Sponge's request): presence in the
page (T-261 → T-258), the Work session list (T-287 → T-282), My machines and the in-use bar (T-314 → T-305), and the
planner's re-assessment of the board (T-280 → T-273). MINOR because the Past sessions dialog is replaced by a panel.
- **Presence everywhere** (`web/presence.js` 1.0.0, R488): a status shape per label, so colour is never the only
  signal (online dot, available dot with a check, away moon, offline ring, working gear, working + available diamond,
  do not disturb a minus), on the DM list, the member column, "Who's here", message authors, the board's assignee
  avatars, the profile card and the user panel. A hover card shows label, since, text, where, do not disturb. "Team
  now" (main menu and chat top bar) lists everyone with label, text, project, task and how long. "My status" sets the
  override with an until, do not disturb, status text, accept work and hide project. A hub without presence shows none
  of it and nothing breaks (the web kit's products).
- **The Work session list** (`web/sessions.js` 1.0.0, R489): a hidable sidebar replaces the Past sessions dialog. It is
  grouped (pinned, folders, today, earlier, hidden, archived) and searchable. Each session has a state dot in colour
  and shape with its meaning on hover (a stalled run shows idle with its reason), its host, AI and project, and at the
  top "3 working · 1 needs you" and the room per host ("Gee's host 3/5"). One menu for "…" and right-click: Open,
  Rename, Pin, Move to folder, Share, Merge into my work, Copy link, Hide, Archive, Delete (asks, force-stops); folders
  have Rename / Collapse / Delete. Switching keeps each session's draft; a session left while it works goes on and its
  reply lands in its own conversation; a 429 keeps the message queued and sends it when a slot frees. /sessions and
  /save open the panel. Hub: the list's summaries carry `queued`.
- **My machines and the in-use bar** (`web/machines.js` 1.0.0, R337): Settings › Connections lists only your own linked
  machines with the scope in words (red for the whole system or unrestricted), a status dot with its own shape, the
  mode, the in-use line and rename / pause / resume / unlink, plus "Link a machine" by code. In Work, a bar says "Using
  the opened folder <name>", or "Using your PC <name>" (red) only when the run really uses wider access. Hub: a run
  records which machine it uses (`GET /api/machines` cards gain `inUse`, built only from the owner's own runs), and a
  run's first call to a machine adds one transcript line, which an older page shows as text. Watchers never get it.
- **The planner re-assesses the board** (team tool `plan_assess`, `bridge/team-mcp.js` 1.5.0 and the native host's
  list, R490 and a library test): proposals only, each with a reason: tasks with no owner, stale ones (14 days),
  claims held by someone offline or lapsed, tasks marked blocked whose block is done (task ids and "n/m" sibling steps
  resolved; the unblock change ready for plan_draft's existingChanges), overlaps, oversized tasks, ideas waiting, an
  unbalanced workload. The planner prompt says when to use it.

## 5.32.0 — 2026-10-27

MINOR: **the board's archive** (T-190, T-226 to T-228). The Done column no longer fills up for good. MINOR because the
first sweep after the go-live archives every task that has been done for more than 48 hours (about 250 on the team
board): they leave the board, nothing is deleted for 30 days.
- **Archiving:** a done task gets `archivedAt` and leaves the board, the lists and the calendar. It can still be read
  (`task_get`, the detail panel says when it will be deleted) and restored, as done or into another column. Moving or
  updating an archived task off "done" restores it too. Only a done task can be archived.
- **The sweep** (the Rust hub, shortly after the start and then every 10 minutes): done tasks finished more than 48 h ago
  go to the archive. A repeating task, one due in the future, or one somebody holds is never archived by it. Tasks
  archived more than 30 days ago are deleted with the usual tombstone (the auto-deploy's data snapshots keep a copy).
  Signed "Board" in the activity log. Settings in `config.json`: `board.archiveAfterHours` (48),
  `board.deleteAfterDays` (30), `board.sweepSeconds` (600); 0 = never.
- **Routes:** `/api/board/archive`, `/restore`, `/archive-done` ("archive all done", one board or all), `/archived`
  (newest first, `q` searches ids, titles and labels, each with `deleteAt`). `GET /api/board` gains `archive`
  (count and the two times); a page without it shows no archive controls.
- **Board tools for Claude:** `task_archive`, `task_restore`, `board_archive_done`, `board_archived`
  (`bridge/board-mcp.js` 1.1.0, and the native AI host's own list, `pgm-ai-internal/src/host/mcp-tools.json`, which
  R164 keeps equal); the Work prompt mentions them.
- **The page** (`web/board.js` 1.1.0, the shared web kit): an Archive button in the board's header (made by board.js, so
  a product's own index.html needs nothing) opens the archive with search, the day each task will be deleted and
  Restore; "archive all done" on the Done column; Archive / Restore in a done task's detail panel.
- **Compatibility:** add-only fields (`archivedAt`, `doneAt`) in the shared board.json. The frozen JavaScript hub shows
  an archived task as done and keeps the fields when it writes. An older page simply shows no archive controls.
- **Checks:** the library's fake-clock tests (nothing before 48 h, what is never archived, deletion after exactly 30
  days with the tombstone, restore, archive all done, the calendar) and **R487** (end to end in the Rust-hub suite:
  people, Claude through the board tools, the board MCP server's tool list, the JavaScript hub reading and writing the
  file).

## 5.31.1 — 2026-10-27

PATCH: **private product releases and Node 24 on the server** (T-249, T-304).
- **`worktree.js deploy-key <owner/name> <key.pub> [title]`:** adds a read-only SSH deploy key to a Forgejo repo, as the
  person whose run it is. It is for a machine that builds or deploys a private repo: the key belongs to no person.
  - Adding the same key again changes nothing.
  - A private key, or a file that is not an SSH public key, is refused before anything is sent.
- **`worktree.js visibility <owner/name> private|public`:** changes only the visibility; nothing is deleted.
- **Check R486:** both commands against the suite's stand-in Forgejo.
- **Done with them on the server (2026-10-27):**
  - the three product repos with downloadable releases (pgm-prodsuit, pgm-hyperprodman, pgm-shadowchat) are private;
  - their deploys read them with one read-only deploy key (Forgejo's SSH is on port 2222; the team note
    plans/release-audit.md has the details).
  - The source-only repos (pgm-libs, VibeRoller, Jcode-PGM) stay public for now: every machine that builds a product
    fetches pgm-libs anonymously.
- **deploy/node-upgrade.md:** the server's Node is 24.22.1 since 2026-10-27. Under vr-run, switching is just the
  `current` link: the hub is Rust, and every Node process (a Work run's MCP helpers, the fallback hub) resolves the link
  when it starts. No service restart is needed.

## 5.31.0 — 2026-10-26

MINOR: **REA in Hyper Workflows** (T-255). A new built-in node kind, `rea`, runs a reverse-engineering step in a
workflow. REA stays one thing: the node uses the same standalone RE tools as everything else (`re_start`, `re_put`,
`re_exec`, `re_call`, `re_get`, `re_stop`), the ones a Work run, Jcode PGM's `/re` and the products already use, and
those keep working unchanged.
- **What a node does:**
  - starts a throw-away copy of the RE image as the run's owner, waiting while it is queued (`queueSec`, default 600);
  - puts `files` in, runs an `exec` command, calls the REA `tool` with `args` (templated, or from the `args` wire);
  - takes the `get` files out;
  - stops the copy.
- **Outputs:** `result` (REA's answer), `text` (its text), `output` (the command's), `files`, and `error` (REA's
  `isError`, a non-zero exit unless `failOnExit: false`, a full queue, a refused tool).
- **Keeping a copy for several steps:** `keep: true` hands the copy on through the `copy` port; the next node works in
  it and `stop: true` ends it.
- **Cleanup:** a copy the node started is deleted on every failure and on a cancel.
- **Where it lives:** in the workflow libraries (`pgm-workflow-internal` for the shape, `pgm-workflow-engine-internal`
  for the behaviour), so any program that runs workflows and has the `re_*` tools runs `rea` nodes.
- **On this hub:**
  - a workflow's `re_*` calls go to the same RE containers as the owner's own, never with admin rights;
  - the RE tools are in the default tool list of a scope that names none (Sponge's decision: anyone on the team may use
    REA, no approval; the hub's cap of 3 copies applies);
  - a scope whose policy lists its tools without them refuses `rea` nodes (`engine.tool-denied`).
- The editor's static kinds list has `rea` (web/editor/kinds-workflow.js 1.1.0).
- **Checks:** six engine tests (`tests/rea.rs`, a stand-in host plays the RE tools), and R485, a real scratch hub running
  a `rea` workflow against the test RE image, with no container left afterwards.

## 5.30.1 — 2026-10-26

PATCH: test only. The Work-prompt test in pgm-ai-internal checks the laws segment only where the hub's LAWS.md exists,
so the same file passes in the pgm-libs checkout. pgm-libs 5.26.0 is re-mirrored from 5.30.0 and has the three
workflow crates. Nothing changes at runtime.

## 5.30.0 — 2026-10-26

MINOR: **Gee's Hyper Workflows branch and Sponge's 5.26.0 to 5.29.1 together on develop.** Both branches had a 5.26.0;
Gee's (work/gee) is written out here, Sponge's further down. Merge notes:
- **Work-run tool relay:** the Hyper Workflows tools (`hyperwork_list`, `hyperwork_run`) go through the per-run secret
  of T-308 like every other run tool: the account and session come from the run's record, never from a header.
- **Team MCP and the host's tool list:** the RE container tools (T-303) and the Hyper Workflows tools side by side.
- **AI host:** the PC role on the library (T-313) and Unity's brain loading (T-293) in the same host.
- versions: project 5.30.0, bridge/team-mcp.js 1.4.0, bridge/team.js 1.1.4, web/index.html 1.27.0.

**From Gee's branch (5.26.0 on work/gee):**

MINOR: **Hyper Workflows** (T-184: T-199 to T-207, T-210, T-186 = T-211 to T-213, T-205; the team note
plans/hyper-workflows.md), **Unity's brain loads on the AI host** (T-293) and the **agent-to-agent spec checks** (T-295).
Everything new is the Rust hub's; the frozen JavaScript hub answers 404 on `/api/hyperwork/` (R445) and keeps reading
every shared file as before (the workflow data is new files under `Hyperwork/`).

**Hyper Workflows: an independent, node-based workflow system.** A workflow is a re-usable flow chart; a prompt sent
through it is carried by the engine through the whole automated back-and-forth (AI calls, JEV decisions, programmatic
steps) until it ends. The AI is one kind of node inside it.
- **Libraries** (native/crates, the library law; they go to pgm-libs with this release): `pgm-workflow-internal` (the
  graph model and JSON document with unknown fields kept and migration on read, port types, validation, diff, versions,
  laws / prompts shapes, node-kind manifests, `{{template}}` and condition expressions, the store; T-199),
  `pgm-workflow-engine-internal` (the engine: tokens on wires, parallel lines and joins, switch, bounded loops,
  sub-graphs, for-each, sub-workflows, retries / timeouts / failure paths, approvals that hold no worker, run limits,
  cancel, an append-only run log that is also the resume memory, replay; `WorkflowHost` + `NodeKind` traits and a fake
  host; T-200), the **built-in kinds** prompt / chat / summarise / jev / tool (+ custom HTTP with secrets in headers
  only) / script (the transform language, no files, network or clock) / agent / orchestrate / sub-workflow, with model
  choice node → section → workflow → host and a fallback (T-201), **laws and saved prompts** applied scope → workflow →
  sections → node, groups, `-law-x`, lockable laws, checks with onFail, `{{prompt:name}}` pinned or latest, export /
  import bundles (T-202), **custom node kinds** made on the fly (graph / http / script; from a selection; published once
  per version after their tests pass; author-only → reviewed → blocked trust; pinned majors; hashes checked; T-203),
  the **decision-model slot** (a scope's JEV through OpenRouter, a local Ollama model as its fallback, a node's own
  model first; T-210), the **AI side chat's server side** (`POST ask`: the person's own AI answers with text and a
  patch of editor commands, writes nothing; T-206), **triggers** (timers every N minutes / hours / days and the board's
  own repeats, missed once or skipped, a scheduler lease so one hub fires, Forgejo / GitHub webhooks signed with the
  trigger's secret, internal events from this machine, standing workflows with run history, pause / resume and a chat
  alert to the owner when a run fails; T-186), and `pgm-workflow-command-internal`, the one `/hyperwork <name>
  [--project P] [--on runner] [--version V] <prompt>` parser for the page, Hyper Prod Man and jcode, with the
  nearest-names helper (T-207; mirrored by copy into Jcode PGM).
- **The hub** (`vr-hub` hyperwork module, `/api/hyperwork/`; T-204): scopes me- / team- / grp- / company (config
  `hyperworkTeams`), rights run / view / edit / publish / admin, run-only clients (a jcode session), list with the
  type-ahead order and `since=<rev>`, get / save (etag, 409) / validate / publish / versions / diff / restore / promote /
  delete / perms / policy / export / import, run (the published version pinned; 422 with the preflight problems), the
  run log streamed as NDJSON with `follow=1`, cancel, answer, replay; runs resume from their logs after a hub restart
  (`run_resume`); the log is scrubbed of secrets (config `hyperworkSecrets`; HTTP only to `hyperworkNetwork`); AI
  through the person's own providers; `POST command` runs a `/hyperwork` line (404 with the nearest names when nothing
  matches); webhooks at `POST /hooks/hyperwork/<id>`, internal events at `POST /internal/hyperwork/event` (config
  `hyperworkEventSecret`, this machine only); **the same routes for a product's people through the gateway**
  (`/internal/gateway/hyperwork/<rest>`, spec 15: Hyper Prod Man's /hyperwork and editor; R474). The team tools
  `hyperwork_list` / `hyperwork_run` let a Work run's AI send a prompt through a workflow as its person (team-mcp.js
  1.2.0, mcp-tools.json). A session keeps the client it signed in with (pgm-identity-internal `session_client`).
- **The page** (web/index.html 1.26.0, web/sw.js 1.0.6): `/hyperwork` and `/hw` in the Work box with a type-ahead from
  the hub's list (grey = cannot run here, with the reason), the run followed line by line, Approve / Reject on an
  approval, the answer at the end; `--project`, else the page's project. **The visual editor foundation** (T-205,
  Sponge's agent branch merged here): `web/editor/core.js` (pure logic: add, move, resize, snap, box select, align,
  group / ungroup, undo / redo, copy / paste across documents, sub-graph views, a json-schema property panel, AI patches
  as one undo step), `view.js`, the plugins `kinds-workflow.js` (the hub's kinds, the document, the `/api/hyperwork`
  client, the run overlay, the side chat with patch preview, triggers) and `kinds-boxes.js` (a T-191 stub), and the
  **Workflows screen** (`workflows-ui.js`: the menu tile, `/workflows`, the list, the editor, save / test run / publish /
  duplicate / delete). Mills' look, the native port (R453) and a few editor gaps (moving a section, opening loop
  sub-graphs, resizing) are left for later.
- **Checks:** R401 to R406 (model), R407 to R423 (engine and kinds), R425 to R436 (laws, custom kinds), R437 to R446 and
  R460 to R462, R465, R472, R474 (the hub over HTTP), R447 to R452 (the editor), R454 to R458 (the side chat), R466 to
  R471 (triggers); 0 failed in both suite modes.

**Unity's brain loads on the AI host** (T-293; Sponge: "my brain glitched: the local brain (unity-brain) is not loaded
on the AI host yet").
- **What happened:** with the hub on the server, Unity borrows Ollama from whichever admin's host lends one. Gee's host
  offered no Ollama (not running there), so the hub asked Sponge's host, whose Ollama has no unity-brain; the engine
  guard rightly stops the hub from loading a model on a host, and only a hub on the same PC ever loaded the brain itself.
- **The host** (vr-host): starts an installed Ollama that is not running (`autoDetect.startOllama`, default on; its
  output in `ollama.log`) and loads the brain from a Modelfile it finds (`unity.modelfile`, else next to `work.hubRepo`,
  in the host folder's `models/unity-brain/Modelfile`, or next to the host folder's parents), then says hello again
  with the new model; one log line when there is no Modelfile. Settings `unity.model`, `unity.modelfile`,
  `unity.ollamaCommand`, `autoDetect.startOllama` (ai-node.example.json).
- **The hub:** with several hosts lending Ollama, Unity asks each one's `/api/tags` (once a minute, or when the set
  changes) and pins the host that has the brain (`AiHosts::prefer_engine_host`); a host whose hello lists the brain
  comes first anyway. When no host has it, she answers on a stand-in chat model of that Ollama (never an embedding
  model; `assistant.fallbackModel` first when set), `/api/assistant/state` says so (`local.standin`) and the Unity screen
  shows it. The glitch text remains only when the Ollama has no chat model at all, and then says how to fix it.
- README (host setup), R473, unit tests for the brain lookup, the stand-in pick, the hello models and the Modelfile search.

**Agent to agent** (T-295 = T-294 · 1/6): the spec is the team note plans/agent-talk.md; its checks R480 to R484 are in
the suite as `spec()` lines (SPEC until T-296 to T-300 build them). The fake Claude's team-tool script and the `HYPERWORK`
script share one relay helper (fake-claude.js).

**Also:** `Ollama` in the AI host's hello (an ai-node.json `unity` block is optional and older hosts need nothing);
NAMES.md lists the three workflow crates; the merge of develop's pgm-plan-internal, releases and notify tools alongside.

## 5.29.1 — 2026-10-26

PATCH: **screen sharing in calls, fixed as far as the code shows** (T-320). Found by reading and testing the code; the
exact report still needs details from whoever saw it.
- **The share tile:** it follows the hub's share flag. It no longer vanishes when a still screen stops sending (Firefox
  mutes such a track), and it no longer waits for an unmute to appear.
- **The share's sound:** kept per person, so a tile that appears after the audio has it. Before, the audio was dropped.
- **After a hub restart:** a quiet re-join keeps the share flag; it used to send only camera and mic.
- **Leaving and re-joining between two polls:** that person gets a fresh connection. A hub restart still keeps the old
  ones.
- **Lost signals:** a lost offer or answer is retried and reported instead of being swallowed. The offerer restarts ICE
  after 5 seconds of "disconnected".
- **Clearer errors:** share errors say what happened (refused, nothing to share or the Linux portal, a capture error,
  plain http).
- **The picker:** a second click no longer opens a second one, and a capture that arrives after leaving is stopped.
- **The Share button:** it explains itself on plain http and in an app whose web view has not switched capture on.
- web/calls.js 1.1.2. Checks R350 to R354, which run calls.js in Node with stubs.

## 5.29.0 — 2026-10-26

MINOR: **the apps run linked machines** (T-313; linked machines step 7 of 8; Linux and Windows only).
- **Install.** One template writes a systemd **user** unit (no root, restarts on failure) and a per-user Windows logon
  task, with `.sh` and `.cmd` twins.
  - `vr-host machine install | uninstall` writes and removes them.
  - The machine refuses to start if its state folder (token, key, scope, audit) can be read by others: group or other bits
    on Linux, a Users / Everyone ACL on Windows.
- **The AI host's PC role is the library's `Machine`.** The host-side `pc_job` is gone: scope, the one canonicaliser,
  audit and the kill switch now apply. The hub still relays to older hosts that only have the PC engine, and there is no
  copy of the canonicaliser outside pgm-net-internal.
- **Desktop app** (VibeRoller and the HPM builds):
  - "Link this computer" runs the device-code link and the machine runner while the app is open.
  - It can install the persistent service.
  - Its local file paths go through the library canonicaliser.
  - Capture stays limited to the hub's own page.
- **Jcode PGM:** `jcodepgm machine link | run | install | uninstall | scope | stop | start | status`, on the mirrored
  library.
- **Windows:** the code is written but unverified here (no mingw). The pgm-net-internal README lists the commands to run
  the same tests on a Windows PC (R339).
- **Checks:** R334, R335, R336, R339. R330's "late ticket" step is now timed from the first call, which made it flaky
  under load.

## 5.28.0 — 2026-10-26

MINOR: **linked machines can be worked, only by their owner; and the PGM gateway carries tools** (agent branches,
reviewed and merged; team note plans/linked-machines.md).
- **Who may reach a machine** (T-310).
  - **One rule:** a call that reaches a person's computer is allowed only when the run's account is the machine's owner
    and that person started the run. This covers a linked machine, the host's PC engine and the browser folder. A new
    add-only `Run.started_by` holds who started it; workflows and agent-to-agent runs must set it.
  - **Refused:** admins, watchers, shared-session viewers, planner sessions and runs started on someone's behalf. Watchers
    never see or answer machine or PC calls.
  - **Machine tokens:** a machine's token reaches only `/api/machine/poll`, `/reply` and `/unlink`. It is no longer a
    login.
  - **Owner away:** the run goes on and ends with a report of what was done, why, and what can be changed.
  - **Compatibility:** a newer scope document is refused whole. The hub's hello says `machine: {v: 1}`. Older apps keep
    their PC engine, and the JavaScript hub keeps the browser PC tools.
  - Checks R327 and R338.
- **The machine's tools** (T-311). When the owner has a linked machine online, the AI's `mcp__pc__*` tools reach it, with
  the same results as today's PC host.
  - **New tools:**
    - `machine_info`;
    - `shell_run`: the working folder is confined to the scope (advisory), it is never run as root, with a timeout and an
      output cap, and it is off under no-shell;
    - `job_start` / `job_read` / `job_stop` for long commands (the job table lives on the machine and ends at revoke);
    - `proc_list` / `proc_kill` (own children only);
    - `re_*` for RE containers on the machine (off by default);
    - `transfer_get` / `transfer_put` for big files by hub ticket (5 minutes, bound to account, machine and path). Data
      moves in 4 MB chunks streamed to disk, with resume and sha256.
  - **Every request** passes one door on the machine, in this order: the kill switch, the hub signature and owner check,
    the scope, the rate limits, the tool, then one audit line.
  - **Order of choice:** the owner's online machine first, then the AI host, then the browser folder.
  - **First app on the new runner:** `vr-host machine link|run|stop|start|status`.
  - Checks R328 to R330 (a 512 MB / 200 MB transfer with resume).
- **The PGM gateway carries tools** (T-317).
  - **Format:** `/v1/chat/completions`, and so the products' gateway, take `tools`, `tool_choice`, assistant `tool_calls`
    and `tool` messages, and return `tool_calls`.
  - **Providers:** OpenAI-compatible providers get them as they are. Anthropic keys and host Claude logins go through
    `tool_use` / `tool_result`. Other kinds answer 400.
  - Streamed requests are answered as chunks. A request without tools is unchanged.
  - Check R346. The products' AI member runs a bounded tool loop in pgm-libs 5.24.0 (not tagged yet); HPM 0.6.0's Hyper
    uses REA's tools through it.
- bridge/pc-tools-mcp.js 1.0.3.
- **Merge fix:** T-310 limited a machine token to poll, reply and unlink, which also blocked the machine's side of a T-311
  transfer (`/api/machine/transfer/<ticket>/…`), so R330 moved 0 MB once both were in. The machine token now also reaches
  the transfer routes; the ticket stays bound to account, machine, path and direction.

## 5.27.2 — 2026-10-26

PATCH: **Work no longer repeats "Done in 0s"** (T-319).
- **The bug:** after a send (most often Send now) the transcript could show `Done in 0s · Opus 5.5` four times in a row.
  Every `result` the page received added a line, and a run can deliver several (empty turns, a result sent again).
- **Hub:** a run says each `result` once: the same uuid arriving again is dropped (Rust hub work.rs; the frozen JavaScript
  hub's server.js the same), so the stream, a page that joins the run again and a share never see a repeat.
- **Page:** one Done / Stopped line per run (a later result of the same run, or one seen before, updates the line, with
  the turn count); a run that ended at once without output says why ("Ended at once: interrupted by Send now" / "you
  stopped it" / the result subtype or error / the stderr tail / "nothing to resume or answer"); identical status lines
  in a row collapse into one with a ×N count.
- Checks R347 to R349; fake-claude gets MULTIRESULT and DUPRESULT.

## 5.27.1 — 2026-10-26

PATCH: **RE base image 1.0.1: REA's Frida tools work** (T-315).
- **The bug:** the `frida` npm package's native binding was never installed, because `npm ci --ignore-scripts` skips
  its install step. So `list_frida_devices` and REA's other Frida tools failed with "Could not locate the bindings
  file". The frida command-line tool was not affected.
- **The fix:** the image now unpacks the official prebuilt `frida-v17.23.2-napi-v8-linux-x64` binding into
  `node_modules/frida/build/`, with its SHA-256 pinned (GitHub's asset digest, matched against a fresh download). No
  install script runs.
- **Smoke test:** a new check calls `list_frida_devices` through REA's MCP server; 28 of 28 pass.
- **Image:** `localhost/pgm-re-base:1.0.1` (2.36 GB) is the new default in pgm-sandbox-internal. 1.0.0 is kept.

## 5.27.0 — 2026-10-26

MINOR: **REA inside Work runs, and linked machines steps 2, 3 and 6** (agent branches, reviewed and merged; team notes
plans/rea.md and plans/linked-machines.md).
- **REA from Work runs** (T-254). Once a run has an RE container, three team tools reach REA inside it:
  - `re_skill{id, file?}`: REA's workflow guide, read from the container, about 9k tokens, only when asked;
  - `re_tools{id, filter?, tool?}`: names only by default (5 KB instead of 47 KB), a search, or one tool's schema;
  - `re_call{id, tool, args?}`: REA's result unchanged, with its untrusted-content marker. Results over 40 KB are saved
    to /work/out.

  How it works:
  - One long-lived MCP session per container (`exec -i`), closed on stop.
  - Owner only; planner sessions are refused.
  - The Work prompt gets one short line; nothing REA-related loads before a container is up.
  - The MCP-over-exec client lives in pgm-sandbox-internal, for Jcode PGM and HPM later.
  - Checks R341 to R345, with a fake MCP server in a tiny test image. Checked by hand against the real image: 229 tools.

  bridge/team-mcp.js 1.3.0, bridge/team.js 1.1.3.
- **Owner-only enforcement** (T-308).
  - **Per-run secret:** each Work run has its own secret for its MCP tools. The hub takes account, session and run from
    the run itself and refuses a request that says otherwise. The old shared secret still works for older run configs,
    logged as deprecated (`legacySharedSecret: false` turns it off).
  - **Owner check:** a call that names a machine reaches it only for its owner. Another person's run, and an admin's,
    get 403 with no fallback.
  - **Signing:** the hub has one ed25519 signing key (`bridge/hub-signing.key`, owner-only) and signs every PC request it
    forwards. vr-host pins the key from its hello (new cap `signed-pc`) and refuses a bad signature, another key, a
    replay, an expired request, another owner or actor, or changed arguments. Older hosts and hubs keep working unsigned.
  - Checks R323 and R324.
- **Linking and revoking machines** (T-309).
  - **Linking:** a computer links to an account with the device code (client `vr-machine`). The approval screen data
    shows what is asking, its key fingerprint and the default scope (one folder). Each machine has its own ed25519 key and
    signs its replies; its token works for that account only.
  - **Routes:** `GET /api/machines` (owner only), rename / pause, unlink. Admins get only `GET /api/machines/count`.
  - **Revoking:** unlinking ends the machine's poll at once, and the machine is told "revoked" and wipes its token. A new
    password or "sign out everywhere" also unlinks the account's machines, unless `keepMachines: true`.
  - **Re-linking** offers "replace the old record" and starts at the default scope.
  - **HPM** keeps its own accounts, so a machine links to each hub separately.
  - Checks R325 and R326.
- **The machine's guards** (T-312; pgm-net-internal::machine):
  - a hash-chained, rotating audit log that never holds file contents (the hub's copy is readable by the owner only);
  - a kill switch (a `KILL` file or `machine stop`) that survives a restart and only the machine can clear;
  - per-class rate limits;
  - admin rights only after approval on the machine, and the app refuses to run as root or Administrator;
  - scope changes: widening from the web waits for approval on the machine, narrowing applies at once and is audited,
    grants expire, and no tool reaches the scope file.

  Checks R331 to R333.
- **Deploy:** `pack-data` and `restore-data` carry `hub-signing.key` on a hub move. restore-data had fallen behind
  pack-data and would have refused work.json, ai-hosts.json and gateway; it now accepts everything pack-data writes.
- **Not yet in use:** the linked-machine parts are library and hub groundwork. A machine does no jobs until T-311, and the
  web panel comes with T-314.

## 5.26.0 — 2026-10-25

MINOR: **reverse-engineering containers, the linked-machines library core, and Node 24 prepared** (agent branches,
reviewed and merged).
- **The RE base image** (T-253; team note plans/rea.md). `native/containers/re-base` builds `localhost/pgm-re-base:1.0.0`
  (2.25 GB, amd64).
  - **What's inside, all pinned and checksum-verified:**
    - REA (`rea-agents` 6.8.0 on Node 24.22.1; installed with `npm ci --ignore-scripts`, `rea setup` never run);
    - Ghidra 12.1.4 and Temurin JDK 21.0.13;
    - Frida 17.24.3, LLDB, GDB, ADB 37.0.1, radare2 6.2.4, strace and ltrace.
  - **Layout:** `/work/in`, `/work/out` and `/work/tmp`, a non-root user, and REA's skill at `/opt/rea/skill`.
  - **Smoke test:** 27 of 27 pass with no network (REA's MCP server lists 229 tools; a JS analysis and a Ghidra headless
    analysis both run).
  - **Scripts:** `build.sh` / `build.cmd`. The Windows build is not tested yet.
- **RE containers** (T-303). New library `pgm-sandbox-internal` 0.1.0, Podman or Docker.
  - **Copies:** each job gets a throw-away copy of the base image. At most 3 run at once (configurable), and the rest wait
    in a FIFO queue.
  - **Control:** only the owner controls a copy; an admin may stop any.
  - **Data:** in and out through exchange folders.
  - **Lifetime:** a copy is deleted entirely on stop. Idle copies are flagged, never killed, and running copies are
    adopted back after a hub restart.
  - **Isolation:** `--cap-drop ALL` plus `SYS_PTRACE` (for lldb, strace and frida), `no-new-privileges`, no network
    unless asked, CPU / memory / pids limits, and no mounts beyond the exchange folders.
  - **Routes and tools:** `/api/re/{start,list,status,exec,put,get,stop}` and the team tools `re_start`, `re_status`,
    `re_list`, `re_exec`, `re_put`, `re_get`, `re_stop`. Rust hub only; the JavaScript hub answers "not available on this
    hub" (bridge/team.js 1.1.2, bridge/team-mcp.js 1.2.0).
  - **Config:** `re: { image, maxConcurrent, idleWarnHours, engineArgs, … }`. On this server rootless podman needs
    `engineArgs: ["--root", "/var/tmp/gee-podman/root", "--runroot", "/var/tmp/gee-podman/run"]`.
  - **Checks:** R316 to R320 run against real podman.
- **Linked machines, step 1 of 8** (T-307; team note plans/linked-machines.md). New module `machine` in pgm-net-internal:
  - **Scope model and evaluator:** ten presets, deny wins, the most specific entry wins, time-limited grants, and a tool
    list per scope.
  - **Path canonicaliser:** one for every tool, with Linux and Windows rules. Symlink and junction escapes are refused,
    and opens are safe against a swapped link.
  - **Safety floor:** the token, the device key, the scope file and the audit log are unreachable under every scope.
  - **Checks:** R321, R322 and R340. Nothing uses it yet.
- **Node 24 prepared** (T-304; deploy/node-upgrade.md).
  - **Why:** the 2026-10-21 security release fixed four high-severity bugs that the server's Node 22.23.3 has.
  - **Tested:** both suites pass on Node 24.22.1 LTS in throw-away hubs. The old Node 22 also still passes, so AI hosts
    on 22 keep working.
  - **Setup:** `deploy/setup-server.sh` installs Node line `VR_NODE_LINE` (24), checked against SHASUMS256.txt.
  - **Not done yet:** the live server's Node is unchanged; the switch is a go-live step.
- **Suite:** R31 (no hashes in tracked files) now skips the container pin files: Containerfile digests and download
  checksums, and the pip and npm locks. They hold published checksums, like native/Cargo.lock.

## 5.25.3 — 2026-10-25

PATCH: **the local Ollama probe works in a browser, and is one shared module** (T-209).
- **Fix:** T-208's browser probe could never work, because the page's CSP `connect-src` did not allow
  `http://127.0.0.1:11434`. The fetch and the "running but blocked" ping were refused, and only the desktop app's path
  worked. The CSP now allows it (R95 and R311 check it).
- **One module:** the probe, the blocked-ping hint, the desktop `ollama_local` call, the tags parser and the "Local (this
  computer)" render helpers move out of the page into `web/localai.js` 1.0.0. It needs no page globals; the page passes
  its hooks in. The same file is in the pgm-libs web kit (5.22.0) and Hyper Prod Man 0.4.0, so there is no second copy.
- web/index.html 1.25.1, web/sw.js 1.0.5. R310 to R314 now read the module.
- **A person's own AI signs as itself outside Work** (T-301 follow-up). The board routes take an optional `asAgent: true`.
  The write is then signed "<person> · Claude" (claim key `<user>#agent`), like a Work run's. It is still that person's
  account and rights, and the field is not stored. Jcode PGM's planner sends it; older hubs ignore it. R315.
- **Suite:** R308 matched a task id as a substring, and the plan note's own header names T-281, which holds "T-28". It
  failed as soon as the second approval's first task became T-28. It now matches the table cell, and the check names
  which of its conditions failed.

## 5.25.2 — 2026-10-25

PATCH: **planner mode is one library** (T-301, the library law).
- **New crate:** `pgm-plan-internal` (pgm-libs 5.23.0, mirrored into native/crates and Jcode PGM's pgm/libs). It holds:
  - the draft check, the parents / sub-tasks table and the approve / change / reject state machine;
  - the writer, in the hub's order, behind a `PlanHub` trait;
  - the planner helpers (plan_parse, plan_existing, plan_roles, plan_owner, plan_glossary, with fallbacks) and the
    helper-set detection.
- **One copy:** pgm-ai-internal's plan.rs and planner.rs are now thin adapters over it (the hub implements `PlanHub` over
  its own stores), and Jcode PGM implements the same trait over the signed-in hub's routes. Nothing is copied any more.
- **Behaviour is unchanged:** R300 to R309 pass. The planner fixtures moved into the crate.

## 5.25.1 — 2026-10-25

PATCH: **the context meter no longer shows 200 %+ on a 1M model** (T-302, Gee's report).
- **What happened:** the hub showed a 200 000-token window until a run's result reported its own window, and believed a
  result that said 200 000 even for a 1M model. A long first run, or a 1M model whose result reports 200k, showed the
  tokens far past the window (200 %+, and "full: start a New chat").
- **Now** the window shown (`/api/usage` `context.window`) is:
  - what the result reported, else the configured default;
  - at least 1M for a model id tagged `[1m]`;
  - never less than what the session already holds: past the window it moves to the 1M tier, and past that to the
    tokens themselves.
- `usage::context_window` is unit-tested. The Jcode PGM side of the same bug is fixed separately.

## 5.25.0 — 2026-10-25

MINOR: **the planner drafts before it writes, with exact helpers, and the person's own Ollama is found** (agent branches,
reviewed and merged).
- **Planner helpers** (T-276, T-277, T-278): five team tools the planner calls instead of guessing.
  - `plan_parse` splits a rough idea list into ideas, each with its model, person, blocked-on and source lines. It
    handles `---` blocks, person sections, a short plus a long version of the same list, and run-on chat.
  - `plan_existing` finds matching open tasks and tasks done in the last 30 days.
  - `plan_roles` and `plan_owner` assign an owner by marker, then by role keywords, then by the lighter workload. They
    never leave the owner empty ("mills to pick" is asked).
  - `plan_glossary` reads plans/glossary.md.
  - The planner prompt now says to use them in that order, and that "blocked on" means based on.
  - The roles are read from plans/roles.md (a json block), then from the Roles part of plans/task-assignments.md, then
    from a built-in list. The note is proposed, not written.
  - Checks R300 to R304.
- **Approve, then write** (T-279, T-281).
  - **The draft:** a planner session shows its whole plan with `plan_draft`. The session turns yellow and the page shows
    a draft card. The person can approve all, approve some parents, ask for a change or reject
    (`POST /api/work/plan/answer`; `GET /api/work/plans` restores open drafts after a reload).
  - **The write:** only then does the hub write, as "<person> · Claude", in this order:
    - the source note, word for word, to plans/<topic>-original.md;
    - the parents, then the sub-tasks named `T-<parent> · n/m`;
    - then the changes to existing tasks. It never deletes or completes a task.
  - **Refused in a planner session:** the AI's own task_create, task_update, task_move, task_delete, claim and complete.
  - **Afterwards:** one plan summary goes to the chat, the owner map is appended to plans/<topic>-plan.md, and the tool
    result lists the new tasks for each owner.
  - Checks R305 to R309; R266 now expects the refusal.
- **Local Ollama** (T-208). The page and the desktop app look for the person's own Ollama and list its models as "Local
  (this computer)" in Settings → AI source and, greyed, in the Work model list.
  - **Desktop app:** asks Ollama directly (`ollama_local`).
  - **Browser:** asks 127.0.0.1:11434. If Ollama runs but the page is not allowed, it shows a one-line `OLLAMA_ORIGINS`
    hint.
  - **When no AI host of theirs lends it:** the desktop app offers "Start this computer's AI host".
  - **Without Ollama:** nothing shows, and the default stays as it was. R310 to R314.
- **Fix:** co-view now starts in the page. web/coview.js's `COVIEW` is a top-level const, not on `window`, so the page's
  `window.COVIEW` check never started it (found while moving co-view into the pgm-libs kit, T-194).
- **Merge fix:** the planner helper dispatcher answered every `plan_*` tool, so `plan_draft` never reached the Work relay
  once both branches were in ("Unknown planner tool"). It now passes `plan_draft` through. R305 to R309 caught it.
- web/index.html 1.25.0, bridge/team-mcp.js 1.1.0.

## 5.24.0 — 2026-10-25

MINOR: **co-view (look at the same thing together) and multi-session Work proven end to end** (agent branches, reviewed
and merged).
- **Co-view** (T-244). Co-view shares the product view itself, not pixels.
  - **How it works:** the sharer's page posts WHICH view it shows (a screen plus an item such as a board task, a note,
    a conversation or a shared Work session; `/api/coview/*`). Each viewer's page opens the same view itself, through
    the normal routes and with its own permissions.
  - **Privacy:** what a viewer may not read reaches them only as "hidden", with no id and no content, and they see "Not
    shared with you".
  - **Controls:** the sharer can invite a viewer to take over and can take the lead back. A viewer can stop following;
    the sharer can end the co-view for everyone.
  - **Older pages and hubs:** an older hub (and the JavaScript fallback) answers "Unknown endpoint", so the buttons stay
    hidden.
  - **Files:** web/coview.js 1.0.0, web/board.js 1.0.6, web/notes.js 1.0.3. R295 to R299.
  - **Still to do:** moving it into the shared kit for Hyper Prod Man and ShaDow.
- **Multi-session Work proven end to end** (T-290). R280 to R284 cover three people on two native hosts with up to 5
  sessions each:
  - the per-user and per-provider limits, and the owner's bypass;
  - a hub restart mid-run (every session comes back as it was);
  - a host killed mid-run;
  - a shared session across a restart;
  - an older page next to the sessions.
  - **Fix found by it:** limits an owner sets from the page are now kept in the host's stored hello, so they survive a
    hub restart. Before, the hub went back to the limits of the host's last hello.
  - **Still to do:** the Windows host run.

## 5.23.1 — 2026-10-25

PATCH: **screen sharing in the desktop app** (T-243, an agent branch, reviewed and merged).
- **Permissions:** the app's main window grants screen capture, microphone and camera only to the hub's own page
  (scheme, host and port must match exactly), through WebView2's permission event on Windows and WebKitGTK's permission
  signal on Linux. On Linux, media streams are switched on for that window only. The Tools window never gets capture.
- **The page can tell:** `app_info` says whether this platform can share (`screenShare`, `screenShareWhy`,
  `screenShareAudio`). Linux needs WebKitGTK 2.38+ with PipeWire and xdg-desktop-portal; the platform matrix is in
  native/crates/vr-desktop/src/capture.rs.
- **Calls:** web/calls.js reads that flag, hiding the Share button and saying why when the app cannot share
  (web/calls.js 1.1.1).
- R290, and R165's smoke check requires the new field.
- **Suite:** native/smoke-desktop.sh lets Xvfb pick a free display itself (`-displayfd`), so suites running side by side
  no longer collide on one display (R165 failed once that way).

## 5.23.0 — 2026-10-25

MINOR: **planner mode, screen sharing with sound, and other tools' sessions** (agent branches, reviewed and merged).
- **Planner mode in Work** (T-275; team note plans/planner-mode.md). A session can be a planner session
  (`POST /api/work/sessions/new {mode: "planner"}`, "New planner session" on the page, a Planner badge, "Leave
  planner").
  - **Tools:** its runs get read-only tools (Read, Glob, Grep; WebFetch / WebSearch and read-only git only if the
    config allows them) plus the board and team servers. No PC tools, and Edit, Write and NotebookEdit are denied; the
    permission mode is forced to "default".
  - **The hub enforces it too:** the relay refuses PC tools, notes_delete and channel / group creation for those runs,
    and notes_write outside `plans/`.
  - **The prompt** carries the planner rulebook, read from the note at run time.
  - Normal sessions are unchanged. R265 to R269.
- **Screen sharing with an audio choice** (T-242). "Share screen" asks for the audio first: none, tab / system audio,
  or that plus the microphone, mixed into one track. The share's sound is a fourth sender, switched on only between
  up-to-date pages (signal field `sa`), so older pages are unaffected. The call line says who is sharing; a shared
  screen can be enlarged (Esc to shrink). Browser differences are documented in web/calls.js (1.1.0). R275 to R279.
- **Other tools' sessions in the list** (T-289, the hub part). `POST /api/work/sessions/report {source, device?,
  sessions[]}` lets a tool (Jcode PGM first) report its sessions in the six states. They are kept in memory for 3
  minutes unless refreshed, and the person's `GET /api/work/sessions` gains `external[]` and `counts.externalWorking`
  (optional fields). R285 to R289.
- Merging: the page's own session (T-238) and planner mode now combine; planner mode's session wins while it is on.

## 5.22.0 — 2026-10-25

MINOR: **my projects, reopen-after-reload, own platform first** (agent branches, reviewed and merged).
- **My projects** (T-239). Settings has a My projects section, a per-person index of the shared projects area.
  - **Per project:** whether you have a worktree, when you last used it, uncommitted / unmerged work, and who else has
    a worktree there.
  - **Pin or hide** a project in your own list (`<profile>/projects.json`, add-only).
  - **Discovery:** the area's other projects and your Forgejo repos are listed, never cloned.
  - **Remove my worktree:** takes away only your own. Uncommitted work is committed first and unmerged work is kept
    under `refs/viberoller/removed/<user>/`; refused while a session of yours works there.
  - **Delete a project:** an admin can delete one that nobody else has a worktree in. It is moved to `_trash` inside
    the projects root, never erased.
  - Routes: `/api/projects/mine`, `/pin`, `/hide`, `/remove-worktree`, `/delete`. R255 to R259.
- **Reopen after a reload** (T-241). A setting, "Reopen what I had open after a reload" (prefs `reopen`, default on).
  Off, a reload starts fresh on the menu and the team room; the conversation stays on the hub. The Rust and the frozen
  JavaScript prefs keep the key (bridge/server.js 4.3.1). R260.
- **Your platform first** (T-251). Downloads shows your own platform first and the others under "Other platforms";
  `/api/downloads` carries a `platform` hint from the User-Agent. The updaters already pick their own OS / arch: the
  desktop app and vr-host ask for their build-time target. R261.

## 5.21.0 — 2026-10-25

MINOR: **start work, and team-only product downloads** (agent branches, reviewed and merged).
- **Start work** (T-238). Opening Work with nothing going on, or the new "New session" button, offers one flow with four
  starts:
  1. pick a project you have;
  2. clone a repo, by address or from a searchable list of the Forgejo repos you can see
     (`GET /api/projects/repos?q=`, run as you; it says when your Forgejo token is missing);
  3. start a new empty project, made private on Forgejo and published so origin is Forgejo
     (`POST /api/projects/create {name}`: repo-create, new, publish; bad name 400, taken 409);
  4. go straight to the AI with no project (a session with `none: true` works in your own scratch folder
     `<projectsRoot>/_scratch/<user>`, with no git and no worktree law).
  Each start opens a new Work session in your own worktree (`sessions/new` makes it at once and returns where it is).
  The page remembers the open session. R245 to R249, with the runner's fake Forgejo now answering repo search, create
  and git smart-HTTP. bridge/worktree.js 2.4.1.
- **Team-only product downloads** (T-250). The Download panel gets a Products section: every product and version
  (ShaDow Chat, Hyper Prod Man, Prod Suit, Jcode PGM; config `releases`), with your platform first.
  - The hub reads the releases from Forgejo with its own token (env `VR_FORGEJO_TOKEN`, else forgejo.json
    `releaseToken` / `token`), caches them for 5 minutes, and streams each file to signed-in people only
    (`GET /api/releases`, `GET /api/releases/file?product&tag&asset`).
  - Only assets in the catalogue can be asked for. No token reaches the page or the log.
  - This lets the release repos go private (plans/release-audit.md). R270 to R274.
- **No flaky restarts in the suite:** R195 and R177 no longer switch the fake build back after a fixed 1.5 s; they wait
  until the restart was seen, and their until-waits go up to 60 s (testing law, rule 2: R195 had failed once under load).

## 5.20.0 — 2026-10-25

MINOR: **presence for AIs and tools** (T-264, an agent branch, reviewed and merged).
- **Full status in the team tools:** `team_presence` and `hub_status` return the whole status: presence, label,
  working and workingSessions, acceptingWork, dnd, text, since, where. The older fields are unchanged.
- **"Tell me when …":** new team tools `notify_when_available {user, what: available | back | online | free,
  message?}`, `notify_list` and `notify_cancel`. A person's Claude asks to be told when someone is available; the asker
  then gets a private message from "<asker> · Claude".
  - Up to 5 per asker, kept for 24 h (Chat/notify-watches.json, a new file).
  - They honour appear offline and never say more than the status shows the asker.
- **A status-change event:** `Chat::subscribe_status()`, which notify uses first and workflow triggers (T-186) can use
  later.
- R250 to R254. bridge/team-mcp.js 1.0.3.

## 5.19.0 — 2026-10-25

MINOR: **the question UI, and device-code sign-in** (agent branches, reviewed and merged).
- **The AI's questions get a real UI** (T-246, T-248).
  - **The series:** one question at a time with Back / Next / Done.
  - **The kinds:** radio buttons, checkboxes, a checklist with select all / none and a filter, "my own answer" fields.
  - **The default:** the recommended answer is marked, with a countdown and "Use the recommended".
  - **Reloads and other devices:** a half-answered series survives a reload. A closed question shows who answered it
    (you, the default, or mixed).
  - **Desktop app:** it notifies when a question arrives in the background.
  - **Settings, "When the AI asks"** (prefs `ask`, add-only): wait N minutes (default 10) then use the recommended
    answers, or never; "any activity waits". The hub takes the wait from the question's own timeoutSec, then the
    person's setting, then config, then 600.
  - **Sound:** a distinct "User action required" tone (`ask`, own volume), plus optional speech. Do not disturb mutes
    sound, speech and notification.
  - R240 to R244. bridge/server.js cleanPrefs keeps the new prefs (4.3.0).
- **Device-code sign-in** (T-236, the hub half). A tool (Jcode PGM first) signs in without a password:
  - **The routes:** `POST /api/device/start` gives a short user code and a link (`?device=CODE`); the person approves
    or refuses it under Settings → Connections → Signed-in devices; `POST /api/device/poll` then gives the tool its own
    token.
  - **The device tokens:** separate from page sessions, kept only as a hash (new file device-tokens.json), listed with
    their last use, and revoked in Settings (`GET /api/devices`, `POST /api/devices/revoke`). A revoked token stops
    working on every route.
  - **Safety:** approving needs the person's own page session; polling, start and wrong codes are rate limited; codes
    expire after 10 minutes; an admin password reset or sign-out revokes the person's devices.
  - R235 to R239.

## 5.18.0 — 2026-10-25

MINOR: **git at a glance, and presence says where people work** (agent branches, reviewed and merged).
- **Git at a glance** (T-240). The Work page has a Git panel for your own worktree of the open session:
  - what it shows: the branch, ahead / behind, and the changed files with stage checkboxes;
  - commit: what is ticked, as you (your Forgejo login if any);
  - Update and Merge: they run only through `bridge/worktree.js`, so Forgejo-first and the versioning law stay in force.
    For a session's own worktree, Merge brings its commits home into your `work/<user>` (`home --session`).
  - Every action is refused (409) while that session works.
  - Routes: `GET /api/work/git` and `POST /api/work/git/{stage,unstage,commit,update,merge}`, with an optional
    `session`. No raw push, no reset. R225 to R229.
- **Where people work** (T-260). Status carries an optional `where` {project, branch, task?, since, source}, from:
  - the person's running Work session (`work`);
  - their held board claim (the task);
  - live reports from other sources (`POST /api/presence {device, source, where}`, e.g. Jcode PGM or the desktop app
    later).
  It is hidden by "hide where" and by appearing offline; the person always sees their own. The team tools carry it too,
  and the page's tooltip reads "on <project> · <task>". Board claims now record when they were made (add-only).
  R230 to R234.

## 5.17.0 — 2026-10-25

MINOR: **the AI asks you, time accounting, and presence on the board** (built by Work-run agents in their own worktrees,
reviewed and merged by the orchestrating run).
- **The AI asks you** (T-245 = T-195 · 1/4; team note plans/question-protocol.md). A Work run calls the team tool
  `ask_user` with a series of single choice, multi choice, checkbox list or text questions, each with a recommended
  answer and "my own answer".
  - The page gets a `bridge_question` event, the asking session turns yellow (`waiting` = question), and the person
    answers through `POST /api/work/answer` (or "use the recommended"; `GET /api/work/questions` lists open ones).
  - The answers return to the AI as one tool result.
  - An unanswered question takes the recommended answers after its timeout (10 minutes by default; `timeoutSec` 0 waits).
  - The relays loop on `{pending, id}` so no HTTP call is held for minutes. A minimal answer card is on the page; the
    full UI is T-246. R210 to R214.
- **Time accounting and an activity log** (T-263; plans/presence.md § 6).
  - **Active time:** kept as spans of activity. A quiet longer than `workGapMs` (config `presence.workGapMs`, default 15
    minutes) ends a span and is not counted.
  - **AI time:** AI working time (`aiMs`, and `agentMs` summed over sessions) keeps adding while the person is away or
    offline. Away time is kept too.
  - **Storage:** per person per day in `profiles/<user>/activity/<day>.json`, with an event log of status changes and
    working sessions.
  - **Route:** `GET /api/presence/time?user=&from=&to=`. The team sees totals; the person sees their own spans and
    events.
  - The page reports input at most once a minute (`active: true`). R205 to R209.
- **Presence on the task board** (T-262). Every card carries the live status of its claim holder (`holder`) and its
  assignee (`assigneeStatus`), and a claim whose holder is offline and not working through a run of theirs is marked
  (`holderOffline`). The board view and `board_list` return `people`: who is available (accepting work and not offline)
  and what each holds. The board MCP tools return the same, so an AI can check before assigning. All optional fields.
  web/board.js shows the holder's label and marks the assign picker. R215 to R219.

## 5.16.0 — 2026-10-25

MINOR: **an AI host sets its own Work-session limits** (T-286, taken over from Gee with Sponge's word; plans/multi-session.md
§ 6). New host settings (ai-node.json, shown and changed in the `vr-host` control app like every other):
`sessions.perUser` (working sessions per person, default 5), `sessions.perProvider` (default 3), per provider
`provider.<id>.maxSessions`, `sessions.exceptions` (per-person limits, `name=n, …`) and `sessions.ownerBypass` (default on:
the owner is not limited on their own host).
- **The host announces them in hello** as `sessionLimits` (optional: an older host sends none and gets the hub's
  defaults; an older hub ignores it). The hub uses them for the 429 of T-285: a person's exception first, then the
  provider's own limit, then the host's; the owner bypass only when the host allows it.
- **Its owner sets them from Settings → My AI hosts**: a new control action `limits` (POST /api/hosts/control
  {id, action: "limits", limits: {...}}, owner only like the other controls). The host saves them in its own settings
  file (the same checks as the control app: 1 to 100, user names) and the hub shows them at once. GET /api/hosts/status
  names each host's `sessionLimits`; /api/work/sessions' per-host counts use them.
- **Page** (web/index.html 1.19.2): the host's live line says its limits; a "Session limits…" button opens a small form
  (per person, per provider, exceptions, "I may exceed them on my own host").
- **Tests:** a unit test of the host settings (defaults, setting, validation, clearing); R204 (a native host in the
  scratch setup announces the defaults, its owner sets new limits from the page, the host saves them, a bad value and a
  non-owner are refused).

## 5.15.3 — 2026-10-25

PATCH: **a session waiting on you** (T-288, the part that has a source today). When a run stops short of a tool it was
not allowed (Claude Code's permission_denials), its session is **waiting** (yellow, which beats red) with what it needs
({kind: "permission", tools}); the next message to that session answers it. The Work page now says so ("Claude was not
allowed to use Bash, so it stopped short. Answer to go on") and plays the mention sound (do not disturb mutes it).
Presence's status gains workingSessions (how many of a person's sessions work at once). Questions (T-195) and plan
approvals (T-273) will set the same waiting state when they exist. R202. web/index.html 1.19.1.
Also: **a share shows the session that is shared.** `POST /api/work/share` takes an optional `session` (none = the first
one, as before); only that session's lines reach the watchers, sharing another session moves the share, and the watch
answer names it and says whether that session is busy. R203.

## 5.15.2 — 2026-10-25

PATCH: **a PC tool call reaches the session that asked.** With several sessions running, a run's PC tool call (the
folder open in the person's browser) went to the first session's run if that one was running, so the page of the
session that asked never saw it. Each session's run now gets its own MCP config naming its session (MILLS_SESSION;
.mcp-runtime-hub-<id>.json), bridge/pc-tools-mcp.js sends it along (an older hub ignores it), and the hub hands the call
to that session's run. R201 (written with the fix; it sends the call while the first session runs with no browser
answering). bridge/pc-tools-mcp.js 1.0.2. R200 failed once in four runs without a recorded reason; its commits now wait
only for a git index.lock (the hub's background freshness check runs git in the same worktrees, the likely cause) and
its detail names each commit's outcome, so a repeat explains itself (testing law, rule 2).

## 5.15.1 — 2026-10-25

PATCH: **a session's worktree follows the worktree law, and its work comes home** (T-292, the rest of it).
`node bridge/worktree.js check | auto | update | home <user> --session <id>`: a session's worktree builds on the
person's own work/<user> (not straight on Forgejo's), so it is STALE (exit 3) when work/<user> has moved on; `update
--session` fast-forwards or merges that in (it refuses with uncommitted changes, changing nothing); `home --session`
merges the session's commits into work/<user> in the person's own worktree ("Merge into my work"; a conflict changes
nothing and says what to do). The hub checks a session's worktree at each of its runs and in the background for
sessions used that day; a session run's system prompt carries its own freshness law and its latest check. New
`POST /api/work/sessions/home {id}` for the session menu. R200. bridge/worktree.js 2.4.0.

## 5.15.0 — 2026-10-25

MINOR: **many Work sessions at once, each in its own worktree, with limits** (T-284, T-292, T-285; team note
plans/multi-session.md). A person has many sessions; each keeps its own conversation + queue, harness session, provider
/ model and project, and several run at the same time (a second message to the SAME session is still 409, R14).
- **The first session** (made from what the person had) keeps its state where it always was (work.json,
  work-current.json): older pages, the frozen JavaScript hub and every route called without a session mean it, exactly
  as before. Other sessions: <profile>/work-sessions/<id>.json; the list in work-sessions.json; the person's folders
  in work-folders.json (all new files, add-only).
- **Each session its own worktree** (T-292): `<user>.<id>` on `work/<user>.<id>`, made at its first run from the
  person's `work/<user>` (pgm-collab-internal `Worktrees::ensure_session`). Its run's system prompt says how its work
  comes home. Deleting a session commits what was left and keeps an unmerged branch under `refs/viberoller/sessions/`
  (`remove_session`). LAWS.md: a line in the worktree law.
- **States** (waiting, working, done-unseen, draft, idle, ready; the first that applies) and the **liveness check**: a run
  that sends nothing of its own for 12 minutes (config workSessions.stallAfterMs) is ended as stalled, so its session is
  idle and frees its slot.
- **Routes:** GET /api/work/sessions (with counts per host), POST /api/work/sessions/new | open | rename | pin | seen |
  hide | move | archive | delete (delete force-stops its run), /api/work/folders (new | rename | delete | order),
  /api/work/draft; /api/chat, /api/stop, /api/work/attach, /api/work/current, /api/work/queue, /api/model,
  /api/workspace and /api/archive/save | open take an optional `session`. The run's first event names its session.
- **Limits** (T-285): only working sessions count, 5 per person per host and 3 per provider on it (config
  workSessions.perUser / perProvider); a host's owner is not limited on their own host; over the limit /api/chat answers
  429 with { error, limit { host, used, max, provider? } }.
- R197 (sessions, worktrees, states, hide, folders, delete) and R198 (limits) were written as spec() first and are plain
  checks now.

## 5.14.0 — 2026-10-25

MINOR: **presence and activity, the hub side** (T-259; team note plans/presence.md). A person is as present as their
most active device: each tab sends its own `device` id on the room long poll and on `POST /api/presence` (an older page
counts as one device, "legacy", as before), so an idle laptop no longer makes an active phone look away. Next to
online / away / offline: **working** (any Work run of theirs; the run's start and end reach pages through the long poll)
and **accepting work** (their own switch). The **label** others see follows plans/presence.md § 2 (Offline · Away ·
Online · Working · Available for work · Working + available). A person can **appear away / offline** (with an until),
set **do not disturb** (it only mutes their own sounds and desktop notifications; nothing is blocked) and a **status
text with an until** (the profile's status line). New: `GET /api/presence` (everyone's status), `GET|POST
/api/presence/me`; the room answer carries `status`; the old `presence` list applies appear offline / away, so older
pages show it too; `hub_status` / `team_presence` gain label, working, acceptingWork, dnd, text. Saved in state.json
"presence" and profile.textUntil (add-only; the JavaScript hub ignores them). The page: a device id per tab, dnd muting
(sounds, read-aloud, desktop and reminder notifications; the settings' test sounds still play). R196 is now a check.
web/index.html 1.19.0, web/board.js 1.0.4.

## 5.13.8 — 2026-10-25

PATCH: **a picture the browser kept no longer makes a page think an older build is running.** Unity's pictures
(/api/assistant/image) are cacheable for a day (private, max-age=86400), and the Rust hub stamped them with its build
(X-VR-Version). After 5.13.3 went live a page showed a picture kept from 5.13.2, read 5.13.2 from it, reloaded "for" it
and then said "VibeRoller 5.13.2+a837cb1 is running, but this page still got 5.13.3+40a831f". The hub no longer names
its build on an answer a browser may keep (pgm-net-internal `http::cacheable`), and the page ignores the build on such
an answer (copies already kept still carry it for up to a day). R199 (written first: it failed with the build on the
picture). web/index.html 1.18.1.

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
