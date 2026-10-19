# VibeRoller on the lab server: the move, step by step

This moves the VibeRoller hub from Gee's PC onto the lab server (the same box as Forgejo). From then on every push
to `main` on Forgejo deploys it, and the hub is up whether or not Gee's PC is. The AI keeps coming from where it is
supplied today: Work runs use Gee's Claude account, logged in once on the server; Unity's local engines stay on Gee's
PC and reach the hub through an **AI node**. With Gee's PC off, everything but Unity's engines keeps working, and
Unity says so plainly.

Nothing here changes a machine by itself. Three scripts do the work; people run them.

| Who | Where | What |
|---|---|---|
| Red (root) | the server | `sudo sh deploy/setup-server.sh --with-nginx` |
| Gee | the server, once | `sudo runuser -u viberoller -- env HOME=/srv/viberoller/home claude login` |
| Red or Gee | Forgejo | the three secrets + two variables the setup script prints |
| Gee | his PC, hub stopped | `deploy\pack-data.cmd`, copy the archive to the server |
| Red (root) | the server | `sudo sh deploy/restore-data.sh <archive>` |
| Gee | his PC | `bridge\ai-node.json` from the example, then `bridge\autostart-windows.cmd` again |

## The files

| File | What it is |
|---|---|
| `deploy/setup-server.sh` | root, idempotent: service user + folders, Node 22 / git / Claude Code check, the live checkout, the server's settings, the systemd service, a deploy key locked to one command, the nginx site (`--with-nginx`). Prints what is left for people. |
| `deploy/pack-data.cmd` / `.sh` | on the old host, hub stopped: packs exactly the data (accounts with their password hashes and device locks, chat, keys, board, notes, pictures, saved sessions, settings). No code, nothing reset. |
| `deploy/restore-data.sh` | root on the server: refuses anything that is not hub data, never overwrites existing accounts, unpacks, hands the files to the service user, counts the accounts, checks Unity's chat key, starts the hub. |
| `.forgejo/workflows/deploy.yml` | push to `main` → one SSH call with the deploy key → the key's one command runs: fast-forward the live checkout, check the scripts parse, graceful restart. Off until the repo variable `VIBEROLLER_DEPLOY` is `on`. |
| `deploy/viberoller-hub.service` | the hub as the locked-down `viberoller` user: writes only under `/srv/viberoller`, no privileges, private temp. |
| `deploy/nginx-viberoller.conf` | TLS, streaming answers unbuffered, WebSocket-ready, `/internal/` blocked, forged forwarding headers dropped. Needs the two `ssl_` lines set to the lab wildcard certificate. |
| `deploy/config.local.example.json` | the server's own hub settings (`bridge/config.local.json`, gitignored): fixed address, no Cloudflare tunnel, behind a proxy, worktrees under `/srv/viberoller`. |

## 1. Server (Red)

```sh
git clone https://git.unityailab.com/GFourteen/VibeRoller.git /tmp/vr && cd /tmp/vr
sudo sh deploy/setup-server.sh --with-nginx
```

Needs root, Node 22 or newer, git, nginx, and `npm install -g @anthropic-ai/claude-code`. The script is safe to run
again. It ends by printing the Forgejo secrets and variables and the remaining people steps. The deploy key it makes
can run exactly one command on the server (the deploy) and nothing else, so the Forgejo secret is not a shell.

## 2. Gee's Claude login (Gee, once)

```sh
sudo runuser -u viberoller -- env HOME=/srv/viberoller/home claude login
```

This puts Gee's Claude login on the server. It is his decision. Work runs then use it, as they use his PC's today.
Until then Work says Claude is not available there; everything else works.

## 3. The data (Gee's approval; a few minutes of pause)

1. Announce it in the team chat. On Gee's PC close the hub window (the knife only pauses the AI).
2. On Gee's PC: `deploy\pack-data.cmd` → `Desktop\viberoller-data-<date>.tar.gz`. Copy it to the server.
3. On the server: `sudo sh deploy/restore-data.sh viberoller-data-<date>.tar.gz`.
4. Sign in at https://viberoller.git.unityailab.com with every account (a right password from a new device answers
   "locked to another device": that proves it without touching the lock).

Passwords, device locks and keys arrive exactly as they were (hub laws 1, 2, 8). `bridge/config.json` is tracked and
`bridge/tunnel.json` belongs to the old tunnel: neither moves.

## 4. Switch the deploy on

Forgejo → `GFourteen/VibeRoller` → Settings → Actions: the secrets `VIBEROLLER_SSH_KEY` (contents of
`/srv/viberoller/deploy-key.b64`), `VIBEROLLER_SSH_HOST`, `VIBEROLLER_SSH_USER`, and the variables `VIBEROLLER_DIR`
and `VIBEROLLER_DEPLOY = on`. The next push to `main` deploys, and the Actions tab shows it.

## 5. Gee's PC afterwards: the AI node

1. `bridge\ai-node.json` from `bridge\ai-node.example.json`: the hub's address, a node name, and the same secret the
   server has under `aiNodes.<name>` in its `bridge/config.local.json` (add it there, then restart the hub).
2. `bridge\autostart-windows.cmd` again: with `ai-node.json` present it now starts the node at sign-in instead of the hub.
3. The heart starts the node, the knife pauses it. With the node paused or the PC off, Unity says her AI host is
   offline; chat, board, notes and Work keep working on the server.

## 6. The web address

The hub serves its own page at https://viberoller.git.unityailab.com. The GitHub Pages copy can keep working: set the
GitHub repo variable `MILLS_HUB_HOST` to that address. The Cloudflare tunnel is no longer needed.
