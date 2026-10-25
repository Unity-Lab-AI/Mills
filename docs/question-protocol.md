# The "AI asks the user" question protocol (T-245 = T-195 · 1/4)

Spec and the first build (2026-10-25). Owner: Sponge (hub, protocol). The page UI is T-246, the 10-minute inactivity
rules and the alert sound are T-195's later parts, the Jcode PGM / HPM side is a later part. This note fixes the names,
so all of them build on one protocol. Checks: R210 to R214 in `bridge/test/regress.js`.

Sponge (T-195): the AI can ask the user single or multi choice questions plus "my own answer"; many questions in a
row with Next, and Done at the end; checkbox lists for longer lists, sent back when done; after 10 minutes with no
activity the recommended answer is taken (on by default, configurable, can be turned off); a distinct alert.

## 1. The AI's side: the tool `ask_user`

An MCP tool of the team server (`bridge/team-mcp.js`; the native host's `vr-host mcp team` lists the same, from
`host/mcp-tools.json`). The run's MCP relay sends it to the hub (`/internal/team`, or `/node/job/<id>/internal/team`
for a run on an AI host), where `work.rs tool()` handles it. It blocks until the question is answered; the answers are
its result. Arguments:

```
{ title?: string,                      // heading for the series
  questions: [ {                       // 1 to 30, in the order to ask (a lone question may also be the arguments themselves)
      id?: string,                     // to find the answer by (default q1, q2, ...)
      text: string,                    // the question
      kind?: "single" | "multi" | "checklist" | "text",   // default: single with options, else text
      options?: [ string | { id?, label, hint? } ],        // the choices (at most 300; a string is both id and label)
      other?: boolean | string,        // offer "my own answer" (a string = its label). Default: on for single and multi, off for checklist
      default?: string | string[],     // the recommended answer: an option id (single), ids (multi, checklist), the text (text)
      required?: boolean } ],
  timeoutSec?: number }                // wait before the defaults are used; default 600; 0 = until answered (max 86400)
```

- **single**: pick one. **multi**: pick several. **checklist**: the same as multi, for a long list the page shows as
  checkboxes (features, items) with select-all. **text**: free text. A series is any list with more than one question:
  the page shows them one after the other (Next, then Done at the end) and sends them back together.
- Unknown options in `default` are dropped. A bad series (empty, no options on a choice, two options or questions with
  one id, over the limits) is refused with `Error: ask_user: <what>` so the AI can fix it and ask again.
- One open question per session at a time (a second `ask_user` while one waits is refused).

### The result

```
{ answered: true, by: "user" | "default" | "mixed", timedOut?: true,
  answers: [ { id, question, kind, picked: [option ids], labels: [labels], other?: "own answer", text?: "text kind", by: "user" | "default" } ] }
```

- `by` per answer: "user" if the person answered that question, "default" if it took its recommended answer (a question
  they left, or the timeout). The top-level `by` is user, default, or mixed.
- A "my own answer" on a single question replaces the pick (`picked: []`, `other` set); on a multi it is added.
- A run that ends while a question is open: the tool gets `Error: Run ended`.

## 2. The hub's side

- **Pending questions** are kept per person in memory (`Work.asks`), each with its session, run key, questions, deadline.
  A question belongs to the session whose run asked: the relay names it (`MILLS_SESSION` for a session that is not the
  first; for a run on an AI host the hub finds the session from the job). Not named = the person's first run.
- **The session** gets `waiting = {kind: "question", at, ref: <question id>, count}` (`sessions::touch`): yellow in the
  list, beating `working` (plans/multi-session.md § 3). It is cleared when the last open question of the session is
  answered, times out or its run ends, and when a new message starts a run. A refused tool (`kind: "permission"`) is a
  different `waiting` and stays.
- **Liveness:** an open question keeps its run alive for the stall check (a person may take longer than `stallAfter`).
- **Long waits:** one HTTP call is never held for the whole timeout. The hub answers `{ok: true, result: {pending: true,
  id}}` after 240 s (`pollSec`, 1 to 240, for tests), and the MCP relay (JS and native) asks again with
  `{poll: <id>}` until the answer comes. The AI sees one call.
- **Timeout:** `timeoutSec` of the call, else `questionTimeoutSec` in config.local.json, else 600. At the deadline the
  recommended answers are used (`by: "default"`, `timedOut: true`). 0 = never. Activity moves the deadline (3).

## 3. The page's side

Events on the run's stream (and so in `GET /api/work/attach` and the replay for a reloaded page; older pages ignore
unknown event types):

```
{ type: "bridge_question", id, session, title, questions: [normalized questions, as in 1 with kind, options [{id,label,hint?}], other, otherLabel?, default, required],
  timeoutSec, at, deadline: ms | null }
{ type: "bridge_answered", id, session, by: "user" | "default" | "mixed", answers }
```

Routes (signed in as the person; allowed during maintenance like `/api/pc-result`):

- `POST /api/work/answer {id, answers: [{id, picked: [option ids], other?: string, text?: string}]}`: answers. A
  question left out takes its recommended answer. Answers are cut down to what the question allows (unknown options
  dropped, one pick for single). 200 `{ok, by}`; 404 not an open question of theirs (also for another person's);
  409 answered already.
- `POST /api/work/answer {id, defaults: true}`: use the recommended answers of all questions ("Use the recommended").
- `POST /api/work/answer {id, activity: true}`: "the person is on it" (any click or key in the question): the
  timeout starts over. 200 `{ok, deadline}`. The page sends it at most every 15 s while someone interacts.
- `GET /api/work/questions[?session=<id>]`: the open questions of the person (`[{id, session, title, questions,
  timeoutSec, at, deadline}]`), for a page that was away or a second device.

The page should: show the question where the session is open and in the list as the yellow dot; sound the alert
("User action required", T-195); keep a typed answer if it reloads (re-read `GET /api/work/questions`); show a closed
question as answered (by whom). The minimum built here (web/index.html 1.19.2): one card with every question (radio /
checkboxes / text, own answer), the recommended answers ticked, Send and "Use the recommended", the activity ping and the
mention sound; the series with Next / Done, checklist styling and the alert are T-246.

## 4. Compatibility

- All fields are optional additions. An older page ignores `bridge_question` and `bridge_answered`; the question then
  waits for its timeout and the recommended answers are used (or a person answers with another device).
- An older AI host's MCP servers do not list `ask_user`, so its runs never ask. A host with the native MCP server gets
  it from its own list and relays it (with `MILLS_SESSION` if set); the long-wait loop is in the relay.
- Nothing stored changes: pending questions live in memory (a hub restart ends the run anyway); `waiting` is an
  existing optional field of the session.
- The frozen JavaScript hub has no `ask_user` handler: its `/internal/team` answers "unknown tool". It is the last
  fallback, not a place to build.
- Windows and Linux: no shell, plain JSON.

## 5. Left for the other parts

- T-246: the page UI (series with Next / Done, checklist with select-all, own-answer fields, alert sound and TTS, the
  session list's yellow dot hover text from `waiting.ref`).
- T-195's 10-minute rule as a setting (per person: on / off / minutes) instead of `questionTimeoutSec` / `timeoutSec`,
  and "any activity waits": today only the question's own interaction (`activity`) restarts the timer.
- Jcode PGM / HPM: the same `ask_user` tool and the same two events in their harness API.
- Planner mode (T-279) asks through this tool.
