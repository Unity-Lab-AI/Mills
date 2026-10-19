# VibeRoller on the lab server: the move, step by step

VibeRoller lives on the lab server (the same box as Forgejo), in a work area the team's accounts share, **without root**:

| Folder | Group | What |
|---|---|---|
| `/srv/apps/VibeRoller` | `VibeRoller` (gee, mills, sponge) | the hub: `app/` (live checkout + its data), `node/` (Node 22 for this account), `tools/` (Claude Code), `home/` (Claude's login + settings), `deploy-ready/` (the nginx files for the root step) |
| `/srv/apps/sharedwork` | `sharedwork` (gee, mills, sponge) | the **projects area**: one folder per project, `_repo/` = its main checkout, one worktree per person; `VibeRoller/` holds the VibeRoller worktrees |

The hub runs as a systemd **user** service of the account that set it up (gee), from boot (lingering on). Every push to
`main` on Forgejo goes live by itself within a minute (`deploy/autodeploy.sh` on a user timer: fast-forward, syntax check,
graceful restart). The AI keeps coming from where it is supplied: Work runs use Gee's Claude login on the server; Unity's
engines stay on Gee's PC and reach the hub through its **AI node**, the VibeRollerAIHost service that starts at boot
(Download → Vibe Roller AI Host). With that PC off, everything but Unity's engines keeps working, and Unity says so.

**The address** is `https://portal.unityailab.com` (also `portal.git.unityailab.com` and `viberoller.git.unityailab.com`).
Anyone can open the sign-in page; without a username, a password and an unlocked device nobody can do anything, and the
hub tells anonymous callers nothing but that a hub is here. No tunnel or proxy is needed.

Nothing here changes a machine by itself. The scripts do the work; people run them.

| Who | Where | What |
|---|---|---|
| gee (or mills / sponge) | the server | `sh deploy/setup-server.sh` (no root; safe to run again) |
| root (Sponge / Red), once | the server | `loginctl enable-linger gee`; the two nginx sites from `/srv/apps/VibeRoller/deploy-ready/`; DNS + certificate for `portal.unityailab.com` |
| Claude, in a Work run on the old hub | Gee's PC | `node bridge/handoff.js pack --to ovh` then `node bridge/handoff.js switch`: data to the server, Gee's PC becomes the AI host (his Claude login stays on his PC; the server never gets one) |
| Gee, optional | his PC | `bridge\host-tools\build.cmd` → Y: the heart / knife on the Desktop and the service that starts the host at boot (without it, the Startup launcher starts it at sign-in) |

## The files

| File | What it is |
|---|---|
| `deploy/setup-server.sh` | as the hub's account, idempotent: user-local Node 22 + Claude Code, the live checkout, `bridge/config.local.json` with a fresh node secret, the hub's git identity, the systemd user services (hub + auto-deploy timer), the nginx site files in `deploy-ready/`, and the printed root / people steps. |
| `deploy/autodeploy.sh` + `viberoller-autodeploy.service` / `.timer` | every minute: a new `main` on Forgejo → fast-forward the live checkout (never anything else), `node --check` every hub script, `bridge/.restart` for a graceful restart. Log: `/srv/apps/VibeRoller/autodeploy.log`. |
| `deploy/viberoller-hub.service` | the hub as a user service: its own Node, Claude Code and HOME under `/srv/apps/VibeRoller`, group-writable files (`UMask=0007`), restart on failure, graceful restarts inside `run.js`. |
| `deploy/nginx-portal.conf` | `portal.unityailab.com` + `portal.git.unityailab.com`: TLS, streaming unbuffered, WebSocket-ready, `/internal/` blocked, forged forwarding headers dropped. |
| `deploy/nginx-viberoller.conf` | the plain name `viberoller.git.unityailab.com`, the same settings. |
| `deploy/config.local.example.json` | the server's own hub settings: fixed address, no tunnel, behind a proxy, worktrees + projects under `/srv/apps/sharedwork`, the AI node secret, Unity's engines via the node. |
| `deploy/pack-data.cmd` / `.sh` | on the old host, hub stopped: packs exactly the data (accounts with their password hashes and device locks, chat, keys, board, notes, pictures, saved sessions, settings). With a host alias it also copies the archive over by `scp`. No code, nothing reset. |
| `deploy/restore-data.sh` | as the hub's account: refuses anything that is not hub data, never overwrites existing accounts, unpacks group-shared, counts the accounts, checks Unity's chat key, restarts the hub. |
| `.forgejo/workflows/deploy.yml` | the optional Forgejo-Actions deploy (a key locked to one command), for a server where the timer cannot run; off unless the repo variable `VIBEROLLER_DEPLOY` is `on`. |

## 1. Server (gee; no root)

```sh
git clone https://git.unityailab.com/GFourteen/VibeRoller.git /tmp/vr && sh /tmp/vr/deploy/setup-server.sh
```

It ends by printing the root steps and the people steps. Run it again any time; it only changes what is missing.

## 2. Root, once (Sponge or Red)

```sh
loginctl enable-linger gee
cp /srv/apps/VibeRoller/deploy-ready/portal.unityailab.com /srv/apps/VibeRoller/deploy-ready/viberoller.git.unityailab.com /etc/nginx/sites-available/
ln -s ../sites-available/portal.unityailab.com /etc/nginx/sites-enabled/
ln -s ../sites-available/viberoller.git.unityailab.com /etc/nginx/sites-enabled/
nginx -t && systemctl reload nginx
```

Then the short name: a DNS `A` record `portal.unityailab.com → 135.148.100.111` (and `AAAA 2604:2dc0:100:386f::`), and
`certbot --nginx -d portal.unityailab.com` (or expand the `unityailab.com` certificate with that name). Done on
2026-10-19: both names answer, the certificate covers them, and the hub's `publicUrl` is `https://portal.unityailab.com`.

## 3. Nobody's login on the server

The server's hub has no Claude of its own (`"hubClaude": false`). Work runs and API answers happen on an **AI host**: Gee's
PC runs the VibeRoller AI Host with his own login (`"defaultProvider": "host:gee"` makes it everyone's default), the hub
hands it the jobs of the people he allows (`"hostGroup": ["mills", "sponge", "gee"]`) and relays the words back. Anyone
can be a host the same way (Settings → My AI hosts), for a Claude login, an Anthropic key or a local model. With Gee's PC
off, Work says so at once and offers the other AIs; chat, board, notes and sign-in never depend on it.

## 4. The data: the hand-off (a Work run does it; a few minutes of pause)

Nothing for Gee to type. In a Work run on the old hub (Claude, from a worktree):

1. Announce it in the team chat.
2. `node bridge/handoff.js pack --to ovh`: the hub packs its own data, pauses its writes (sign-ins, chat, board and notes
   answer "moved to https://portal.unityailab.com"), copies the archive to `/srv/apps/VibeRoller/` and runs
   `deploy/restore-data.sh` on the server. It prints the account count and "the hub is up with the data".
3. `node bridge/handoff.js switch` as the very last step: from its next graceful restart (and at every start after it)
   Gee's PC runs the AI host instead of the hub. The Work run ends with the old hub.
4. Sign in at the portal with every account (a right password from a new device answers "locked to another device": that
   proves it without touching the lock). `node bridge/handoff.js undo` on Gee's PC brings the old hub back if ever needed.

By hand instead: close the hub window, `deploy\pack-data.cmd ovh`, then `restore-data.sh` on the server. Passwords, device
locks and keys arrive exactly as they were (hub laws 1, 2, 8). `bridge/config.json` is tracked and `bridge/tunnel.json`
belongs to the old tunnel: neither moves.

## 5. Gee's PC afterwards: the AI host

Settings → Connections → **My AI hosts** → **New host** (the hub issues a nickname and a UUID) → **Download Windows**: the
package has `ai-node.json` filled in (hub address, id, secret, user, a Claude provider shared with the group, Unity's engine
addresses). Unzip it somewhere permanent, run `host-tools\build.cmd` and say **Y** to the service. From then on the
**VibeRollerAIHost** service starts at boot and his PC is the team's Claude (and Unity's engines); the heart and the knife
on the Desktop start and stop it. (Gee's checkout works the same way: `bridge\ai-node.json`, `bridge\host-tools\build.cmd`.)
The legacy `aiNodes.gee-pc` secret in the server's `config.local.json` keeps working for the engines-only config.

## 6. Deploys

Push to `main` on Forgejo; within a minute the server has it (`/srv/apps/VibeRoller/autodeploy.log`, and the version in
Settings). The GitHub Pages copy is no longer the address; it keeps deploying but points at the tunnel-era hub lookup.
