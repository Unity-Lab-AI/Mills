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

**The address** is the private front door `https://portal.unityailab.com` (today also `https://portal.git.unityailab.com`,
which the lab's wildcard certificate already covers). nginx shows the hub's sign-in only to connections that come from the
server itself; everyone else gets a 404 page: *These are not the droids you are looking for.* To be "inside", open an SSH
tunnel and use it as a SOCKS proxy: `ssh -D 1080 ovh`, then point the browser (or a browser profile) at
`socks5://127.0.0.1:1080`. A VPN that ends on the server works the same way once its range is in the site file.

Nothing here changes a machine by itself. The scripts do the work; people run them.

| Who | Where | What |
|---|---|---|
| gee (or mills / sponge) | the server | `sh deploy/setup-server.sh` (no root; safe to run again) |
| root (Sponge / Red), once | the server | `loginctl enable-linger gee`; the two nginx sites from `/srv/apps/VibeRoller/deploy-ready/`; DNS + certificate for `portal.unityailab.com` |
| Gee | the server, once | `HOME=/srv/apps/VibeRoller/home CLAUDE_CONFIG_DIR=/srv/apps/VibeRoller/home/.claude /srv/apps/VibeRoller/tools/bin/claude login` |
| Gee | his PC, hub stopped | `deploy\pack-data.cmd ovh` (packs the data and copies it over) |
| gee | the server | `sh /srv/apps/VibeRoller/app/deploy/restore-data.sh /srv/apps/VibeRoller/viberoller-data-<date>.tar.gz` |
| Gee | his PC | Download → Vibe Roller AI Host (Windows): `ai-node.json`, `build.cmd`, Y to the service |

## The files

| File | What it is |
|---|---|
| `deploy/setup-server.sh` | as the hub's account, idempotent: user-local Node 22 + Claude Code, the live checkout, `bridge/config.local.json` with a fresh node secret, the hub's git identity, the systemd user services (hub + auto-deploy timer), the nginx site files in `deploy-ready/`, and the printed root / people steps. |
| `deploy/autodeploy.sh` + `viberoller-autodeploy.service` / `.timer` | every minute: a new `main` on Forgejo → fast-forward the live checkout (never anything else), `node --check` every hub script, `bridge/.restart` for a graceful restart. Log: `/srv/apps/VibeRoller/autodeploy.log`. |
| `deploy/viberoller-hub.service` | the hub as a user service: its own Node, Claude Code and HOME under `/srv/apps/VibeRoller`, group-writable files (`UMask=0007`), restart on failure, graceful restarts inside `run.js`. |
| `deploy/nginx-portal.conf` | `portal.unityailab.com` + `portal.git.unityailab.com`: TLS, insiders (the server's own addresses) → the hub, everyone else → the droids 404; streaming unbuffered, WebSocket-ready, `/internal/` blocked, forged forwarding headers dropped. |
| `deploy/nginx-viberoller.conf` | the plain name `viberoller.git.unityailab.com` (no inside / outside rule), same proxy settings. |
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
`certbot --nginx -d portal.unityailab.com` (or expand the `unityailab.com` certificate with that name). Until then
`https://portal.git.unityailab.com` is the address, and `publicUrl` in `bridge/config.local.json` should say so.

## 3. Gee's Claude login (Gee, once)

```sh
HOME=/srv/apps/VibeRoller/home CLAUDE_CONFIG_DIR=/srv/apps/VibeRoller/home/.claude /srv/apps/VibeRoller/tools/bin/claude login
```

This puts Gee's Claude login on the server. It is his decision. Work runs then use it, as they use his PC's today.
Until then Work says Claude is not available there; everything else works.

## 4. The data (Gee's approval; a few minutes of pause)

1. Announce it in the team chat. On Gee's PC close the hub window (the knife only pauses the AI).
2. On Gee's PC: `deploy\pack-data.cmd ovh` → `Desktop\viberoller-data-<date>.tar.gz`, copied to `/srv/apps/VibeRoller/`.
3. On the server: `sh /srv/apps/VibeRoller/app/deploy/restore-data.sh /srv/apps/VibeRoller/viberoller-data-<date>.tar.gz`.
4. Sign in at the portal with every account (a right password from a new device answers "locked to another device": that
   proves it without touching the lock).

Passwords, device locks and keys arrive exactly as they were (hub laws 1, 2, 8). `bridge/config.json` is tracked and
`bridge/tunnel.json` belongs to the old tunnel: neither moves.

## 5. Gee's PC afterwards: the AI host

Download → **Vibe Roller AI Host (Windows)** on the site. Unzip it somewhere permanent, copy `ai-node.example.json` to
`ai-node.json` with the `gee-pc` secret from the server's `bridge/config.local.json`, run `host-tools\build.cmd` and say
**Y** to the service. From then on the **VibeRollerAIHost** service starts at boot; the heart and the knife on the Desktop
start and stop it. (Gee's checkout works the same way: `bridge\ai-node.json`, `bridge\host-tools\build.cmd`.)

## 6. Deploys

Push to `main` on Forgejo; within a minute the server has it (`/srv/apps/VibeRoller/autodeploy.log`, and the version in
Settings). The GitHub Pages copy is no longer the address; it keeps deploying but points at the tunnel-era hub lookup.
