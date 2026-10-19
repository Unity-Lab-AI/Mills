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

## Nothing breaks, and nothing is lost

- The server's auto-deploy (`deploy/autodeploy.sh`) snapshots the hub's data before every deploy and checks that the new
  version comes up. If it does not, it goes back to the last good version by itself and says so. Deploys never touch
  data (accounts, chat, board, notes, pictures are gitignored).
- Hub changes stay compatible with older AI hosts: a host that has not updated keeps working. A host never has to update
  for the hub to keep working; updates reach hosts by themselves through the hub (a host can switch that off).
- Before going live: the regression suite reports 0 failed, and afterwards the deploy log is watched until the new
  version is confirmed up.
