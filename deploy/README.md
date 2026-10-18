# VibeRoller on the lab server: hand-off for Red

This kit moves the VibeRoller hub from Gee's PC onto the lab server (the same box as Forgejo), so that every
push to `main` on Forgejo deploys it live. The hub and all the work on VibeRoller then live on the server.
The AI keeps coming from where it is supplied today: Work runs use Gee's Claude account, logged in once on the
server. Unity's local engines stay on Gee's PC (an AI node, a later step). Nothing here changes a box by
itself. Every step below is done by a person.

| File | What it is |
|---|---|
| `.forgejo/workflows/deploy.yml` | push to `main` → SSH to the server → fast-forward the live checkout → check the scripts parse → graceful restart. Off until the repo variable `VIBEROLLER_DEPLOY` is `on`. |
| `deploy/viberoller-hub.service` | systemd unit: the hub as the locked-down `viberoller` user, writing only under `/srv/viberoller`. |
| `deploy/nginx-viberoller.conf` | nginx site for `viberoller.git.unityailab.com`: TLS, streaming answers unbuffered, WebSocket-ready, `/internal/` blocked, no client-forged forwarding headers. |
| `deploy/config.local.example.json` | the server's own hub settings (`bridge/config.local.json`, gitignored): fixed public address, no Cloudflare tunnel, behind a proxy. |

## 1. The work area (once, as root)

```sh
useradd --system --home-dir /srv/viberoller/home --create-home --shell /usr/sbin/nologin viberoller
mkdir -p /srv/viberoller/{app,worktrees,home} && chown -R viberoller:viberoller /srv/viberoller && chmod 750 /srv/viberoller
# Node 22 + git + Claude Code, available to every user
node --version   # must be 22 or newer
npm install -g @anthropic-ai/claude-code
```

The `viberoller` user has no shell, no sudo and no access to Forgejo's files. The service may only write under `/srv/viberoller`.

## 2. The live checkout (as the viberoller user)

```sh
sudo -u viberoller git clone https://git.unityailab.com/GFourteen/VibeRoller.git /srv/viberoller/app
sudo -u viberoller cp /srv/viberoller/app/deploy/config.local.example.json /srv/viberoller/app/bridge/config.local.json
sudo -u viberoller git -C /srv/viberoller/app config user.name "VibeRoller hub"
```

Work runs make their worktrees in `/srv/viberoller/worktrees` and push to Forgejo. They need push rights to the repo: give the `viberoller` user a Forgejo access token (`~/.git-credentials`), or let each person use **My Forgejo** in Settings.

## 3. Gee's Claude login (Gee, once)

```sh
sudo -u viberoller -H env HOME=/srv/viberoller/home claude login
```

This puts Gee's Claude login on the server. It is Gee's decision. Work runs then use it, as they use his PC's today.

## 4. Moving the data (only with Gee's approval; a few minutes of pause)

1. Announce it in the team chat, then stop VibeRoller on Gee's PC (the knife only pauses the AI, so close the hub window).
2. Copy these from Gee's `Mills` folder into `/srv/viberoller/app/`, **exactly as they are**, then give them to the viberoller user (`chown -R viberoller:viberoller`). Passwords are never reset or changed (hub laws 1, 2, 8):
   - `bridge/auth.json`, `bridge/state.json`, `bridge/hub-state.json`, `bridge/profiles/` (accounts, device locks, settings, Work state)
   - the other `bridge/*.json` runtime files (`providers.json`, `forgejo*.json`, `server-owners.json`, `usage.json`, `assistant.json`) and `bridge/profiles-deleted/` if it exists
   - `Chat/` (the encrypted chat, everyone's public keys `keys.json`, and Unity's own chat key `agent-key.json`: without it Unity can no longer read the chat), `Archive/`, `Board/`, `Notes/`, `Avatars/`, `Images/`, `Api/`
   - Not `bridge/config.json` (tracked) and not `bridge/tunnel.json` (the old tunnel).
3. Start it: `systemctl enable --now viberoller-hub`, then `curl -s http://127.0.0.1:7777/api/status`.
4. Sign in at https://viberoller.git.unityailab.com and check every account (a correct password from a new device answers "locked to another device", which proves it without touching the lock).
5. Gee's PC: keep the hub off there from now on. Remove the autostart (`bridge\autostart-windows.cmd remove`) so two hubs never run against the same people.

## 5. nginx

Install `deploy/nginx-viberoller.conf` (header of the file), point the two `ssl_` lines at the lab wildcard certificate, `nginx -t`, reload.

## 6. Switch on the deploy

1. Make an SSH key for the `viberoller` user and add its public half to `/srv/viberoller/home/.ssh/authorized_keys`. The service user's shell is nologin, so give the key a forced command or a restricted shell that allows `sh -s` in `/srv/viberoller/app`, or use a separate deploy user that may run the same commands as `viberoller` through a narrow sudoers rule.
2. Forgejo → `GFourteen/VibeRoller` → Settings → Actions: secrets `VIBEROLLER_SSH_KEY` (base64 of the private key), `VIBEROLLER_SSH_HOST`, `VIBEROLLER_SSH_USER`, variables `VIBEROLLER_DIR` = `/srv/viberoller/app` and `VIBEROLLER_DEPLOY` = `on`.
3. Push anything to `main`. The Actions tab shows the deploy, and the hub restarts gracefully.

From then on: Work's "go live" (`node bridge/worktree.js live`) on the server merges into `main` and pushes. The push deploys. Nobody signs out.

## 7. The web address

The hub serves its own page at https://viberoller.git.unityailab.com and tells it its address (`publicUrl`). The GitHub Pages copy can keep working too: set the GitHub repo variable `MILLS_HUB_HOST` to `https://viberoller.git.unityailab.com`. The Cloudflare tunnel is no longer needed on the server.
