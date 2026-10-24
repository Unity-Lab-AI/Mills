# VibeRoller team laws (tracked)

Every Work run gets this file, followed by the host's own `CLAUDE.md` (the numbered hub laws 1–15, which predate this
file and stay in force). These laws are in the repo, so every machine and every worktree has the same ones.

## Worktree law: never build on a stale worktree

Sponge, 2026-10-19: *"if our worktrees are stale, a law that needs to be followed is that we need to check if things are
stale, old, etc, and have the ability (because worktrees are based on a repo) to update the repo, and make a new
worktree, in such a way that if we have modifications from our old worktree, we can easily bring over those
modifications to the new worktree, then abandon and delete just the old worktree."*

- **Check before changing anything:** `node bridge/worktree.js check <username>` (add `--project <name>` for other
  projects). It fetches Forgejo first. Exit code 3 means STALE: behind Forgejo's integration branch, or on the wrong
  branch (a worktree belongs on `work/<username>`).
- **When it is stale:** `node bridge/worktree.js update <username>`. It fast-forwards a clean worktree, merges Forgejo's
  integration branch into your own commits, or renews the worktree.
- **Renew** (`node bridge/worktree.js renew <username>`) makes a NEW worktree from Forgejo's integration branch and
  carries everything of the old one over: your own commits, uncommitted changes and untracked files. Only when all of it
  came over is the old worktree deleted. If something did not come over, the old one stays as `_old-<username>-<time>`
  next to the new one, with the reason. Either way its state is kept under `refs/viberoller/renewed/`, so nothing is
  ever lost.
- The hub checks every worktree in the background (when a run starts, when you switch projects, and every 5 minutes for
  anyone active that day). It fast-forwards a clean one by itself and tells the next run what it found.
- Never delete or reset someone else's worktree. Never `git reset --hard` or force-push to get out of a stale state:
  use update / renew.

## Forgejo is the source of truth

- Forgejo (`origin`) holds what everyone builds on. Work happens on several machines, so local branches go stale;
  `worktree.js` fetches before it reads or writes an integration branch.
- `merge` and `live` push to Forgejo FIRST and change nothing locally when that push fails. Nothing goes live that
  Forgejo does not have. A server-only `main` once restarted the hub every minute and every AI host lost its line.
- Pushing as yourself needs your Forgejo account connected (Settings → My Forgejo). Your Work runs then push as you.
- Your Work runs also have `fj` (the Forgejo CLI) signed in as you, next to git: your token in a keys file of your own,
  no `fj auth` needed (a run of yours never acts as anyone else). `node bridge/worktree.js repos [words]` lists the
  repos you can see (private ones too), `repo-create <owner/name>` makes one, `publish <owner/name> --project <name>`
  puts a project on Forgejo (a GitHub origin is kept as `upstream`, origin becomes Forgejo, every branch and tag goes
  up, a `develop` is made), `tag <name> [message]` tags the integration branch on Forgejo. On the server fj is the
  pre-built forgejo-cli binary under tools/bin (deploy/setup-server.sh).

## Versioning law (replaces how hub law 14 picks the bump; the rest of law 14 stands)

Sponge, 2026-10-20: *"we don't want to scale the first number nearly as quickly as the second number, and the second
number should scale less than the third number, however, this is all dependent on WHAT we are doing."*

- **MAJOR (X.0.0):** an entire refactor or rework, or a very large change where 50 % or more of the codebase is changed
  or added to.
- **MINOR (x.X.0):** moderate to large new features, or changes that MIGHT cause breaking or minor issues (not certain
  to, but could).
- **PATCH (x.x.X):** fixes, minor features, small changes: anything not moderate to large.
- A higher number going up resets the lower ones to 0: 1.5.3 → 1.6.0, 1.5.3 → 2.0.0. There is no maximum
  (50.999.3953 is fine). The first number moves far less often than the second, the second less than the third.
- **Bump before merging a feature into develop.** Every merge moves the version above what develop has now;
  `worktree.js merge` refuses otherwise and names the right next numbers. If two people worked from the same version
  (both on 1.5.3), the second merge bumps again past the first: 1.5.5 instead of a second 1.5.4, or 1.7.0 instead of a
  second 1.6.0.

## Compatibility law

- **Windows and Linux, always.** Everything that runs on a machine (hub, AI host, launchers, desktop app, scripts) works
  on both; a launcher or script has its `.cmd` / `.bat` and its `.sh`.
- **Forwards and backwards.** A newer hub works with older AI hosts, older pages and older apps; a newer host works with
  an older hub. A feature that needs the newer side may be missing or look wrong on the older one, but everything else
  keeps working. Only something deliberately changed may stop working the old way (and that is a MINOR or MAJOR bump
  with its reason in the CHANGELOG). New protocol fields are optional, unknown fields are ignored, and a capability is
  announced (a host's `caps`) before it is relied on.
- **Everything updates itself.** The web page reloads itself onto a new hub version as soon as nothing is in progress.
  The desktop app is the same page in its own window. AI hosts update from the hub (automatic, between answers, and by
  hand with `node bridge/ai-node.js --update`); a manual update is always possible too (Download panel, or `git pull`).

## Going live never cuts anyone off

- A go-live or update must not break the AI host, ever: Gee's host is usually what everyone's Work runs on, and it
  tends to break right when he is leaving. Before going live, ask: does a host that has not updated still connect, keep
  its session and answer? (R154 checks a hub restart keeps every host's session.)
- Hosts and hub watch each other: the host's poll is a heartbeat both ways (the hub's answer carries its own beat). Both
  log when the other went away and for how long, and reconnect within seconds as if nothing happened; `hub_status`
  shows each AI host up or down and since when.
- A host only restarts or updates when nothing is in flight and nothing came for 3 minutes. A host in steady use never
  gets those 3 minutes, so after an automatic update has waited 30 minutes it takes the next moment with nothing in
  flight (5.7.2): the restart takes seconds, the hub keeps the session (R154) and holds a job that arrives meanwhile for
  up to 40 s until the host picks it up.
- Swapping what runs the hub (run.js → vr-run, the JavaScript hub behind → the Rust hub alone, 5.6.0) is one hub
  restart and nothing else for a host: the host link (`/node/*`) has been the Rust hub's since 4.0, every host keeps
  its session across a restart (R154), and a host that has not updated keeps working (R152). Do it at a quiet moment
  (`/api/status` from the server says `busy: false`, hub_status shows no shared Work session), announce it first, and
  watch `/api/status` come back; vr-run's own fallback (the previous good build; for the first swap also the
  JavaScript hub) covers a bad build without anyone's hand.

## The JavaScript hub stays as the last fallback

Sponge, 2026-10-24: *"we leave JavaScript as a fallback … so that while a lot of newer stuff might not work if it falls
back to JavaScript, we still have something that works enough to fix it and keep working. Moving forward we need at
least 1 if not 2 Rust fallbacks when we get to stable versions, so that we only use the non-updated JavaScript version
if all else fails."*

- **The order of service** (vr-run with `rustHub.alone`; `/api/status` shows it as `serving`): the newest Rust build
  (`rust`), the previous good build (`rust-fallback-1`), the one before (`rust-fallback-2`), and only when all of those
  fail the JavaScript hub alone (`javascript-fallback`), said loudly in the log, the chat and /api/status. The Rust hub
  serves alone; keep-builds.sh keeps the current build + 2 good ones, always.
- **JavaScript is frozen, not removed.** bridge/server.js (and what it needs) is the last resort. A new feature is built
  in Rust and does not have to exist in JavaScript. But the JavaScript hub must keep booting and serving the basics on
  every tree we ship: sign-in, chat, the board, Work with Claude, the AI host link, so a broken release can be fixed
  from it. The suite keeps the check (R192, `node bridge/test/js-fallback.js`); it must stay green.
- **Shared data stays readable by it.** Every change that touches a data file both hubs read (accounts, sessions, chat,
  board, notes, work.json, ai-hosts.json …) keeps the JavaScript hub able to read it: add fields, never rename or
  re-shape what is there; a Rust-only file is fine, a changed shared file is not.
- **Before every go-live the run checks the fallback chain** (two good Rust builds kept, the JavaScript hub boots), and
  it never removes the JavaScript hub without Sponge's explicit word.
- Going back from `alone` to the JavaScript hub behind the Rust one is one config flip and a graceful restart
  (deploy/full-rust-swap.md).

## Finish the whole job, test it in throw-away hubs, go live once

Sponge, 2026-10-24: *"we dont restart vibe roller, and use throw-away tests, until ALL sub agents are done, and all work
is done … it's better for us to accumulate changes until everything is done and complete, in all parts … we should go
live with more changes at once, that have been tested in throwaway, compared to going live a bunch of times during the
middle of development."*

- **Accumulate, then go live once.** Work in progress is committed on `work/<username>` (and the work branches of the
  other projects) and stays there. Nothing is merged into develop or taken live while any part of the job is still
  being built: not "most of it is done", all of it. Then one merge, one go-live, with every part in it.
- **Test in throw-away hubs only.** The regression suite (`node bridge/test/regress.js --no-prod`, normal and
  `--rust-hub`) runs its own scratch hub on its own ports with its own data, and is the place to try a change. The
  live hub is never restarted to try something, and nothing is shipped to see whether it works.
- **A run never ends while its agents work.** A Work run is one Claude process per message; it ends when the reply is
  sent, and every background agent it started dies with it, mid-edit (2026-10-24: this, not the hub, cut the agents
  off again and again all day — the live hub ran without a restart from 04:46). So a run that has started agents keeps
  working until each of them has reported, has been reviewed, tested and committed, before it sends its last message.
  It tells the person it is still going (a short progress line is fine) instead of ending the turn. When an agent is
  cut off anyway, the next run resumes it from its transcript first, before anything new.
- **One go-live at a time, at a quiet moment.** When the whole job is done and both suite modes are 0 failed on the
  final tree: announce it, merge, go live, and stay in the run until the deploy log says the new version is verified
  (the graceful restart waits for the shipping run to end, so the check is the next run's first step if it has not
  happened yet). Then the AI hosts update by themselves.

## Library law: reused code lives in a library

Sponge, 2026-10-22: *"new libraries (and existing libraries) need to be developed or extended when there is code that
is going to be re-used amongst projects, to help reduce and re-use when possible."*

- Code that two projects need (the hub, the AI host, a PGM product, Jcode PGM, a plugin) is written once, in a
  library, and used from there. Before writing it, look for an existing crate or module that already does it or
  nearly does; extend that one rather than start a copy.
- The libraries are **pgm-libs** (`pgm-core`, `pgm-identity`, `pgm-hcom`, `pgm-ai`, `pgm-collab`, `pgm-dist`,
  `pgm-net`, `pgm-product`; mirrored from `native/crates/*-internal`) and what grows next to them (the Jcode PGM
  plugin SDK and host, the harness interface). A new library is right when the code has no home in those; a new
  crate inside a product is not (it cannot be reused from outside).
- Copying a function between projects is the signal: stop, move it into the library, bump the library, use it from
  both. The JavaScript hub's twin copies were the cost of not doing this (the "4.1.0 is ready" popup was a two-copy
  bug).
- A library change follows the product release checklist: bump its version, regenerate the lock, build `--locked`,
  tag on Forgejo, then bump the users.

## Nothing breaks, and nothing is lost

- The server's auto-deploy (`deploy/autodeploy.sh`) snapshots the hub's data before every deploy and checks that the new
  version comes up. If it does not, it goes back to the last good version by itself and says so. Deploys never touch
  data (accounts, chat, board, notes, pictures are gitignored).
- Hub changes stay compatible with older AI hosts: a host that has not updated keeps working. A host never has to update
  for the hub to keep working; updates reach hosts by themselves through the hub (a host can switch that off).
- Before going live: the regression suite reports 0 failed, and afterwards the deploy log is watched until the new
  version is confirmed up.
- One request's failure is that request's alone (5.4.0): the Rust hub answers it 500 and logs it; it never aborts,
  every lock recovers from a poisoning, and a background loop that fails is started again. The full-Rust hub (no
  JavaScript hub behind it) is specified in the team note plans/full-rust-hub.md: each step has spec checks in the
  suite (`spec()`, R174–R179) that say what the hub SHOULD do before it is built.
