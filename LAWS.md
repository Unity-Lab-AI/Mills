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
- A host only restarts or updates when nothing is in flight and nothing came for 3 minutes.

## Nothing breaks, and nothing is lost

- The server's auto-deploy (`deploy/autodeploy.sh`) snapshots the hub's data before every deploy and checks that the new
  version comes up. If it does not, it goes back to the last good version by itself and says so. Deploys never touch
  data (accounts, chat, board, notes, pictures are gitignored).
- Hub changes stay compatible with older AI hosts: a host that has not updated keeps working. A host never has to update
  for the hub to keep working; updates reach hosts by themselves through the hub (a host can switch that off).
- Before going live: the regression suite reports 0 failed, and afterwards the deploy log is watched until the new
  version is confirmed up.
