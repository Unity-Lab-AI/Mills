#!/usr/bin/env sh
# VibeRoller: make the lab server the hub's home. Run as root, once; safe to run again (every step checks first).
#
#   sudo sh deploy/setup-server.sh [--with-nginx] [--dir /srv/viberoller] [--host viberoller.git.unityailab.com]
#                                  [--repo https://git.unityailab.com/GFourteen/VibeRoller.git] [--user viberoller]
#
# It: makes the locked-down service user and folders · checks Node 22, git and Claude Code · clones (or fast-forwards)
# the live checkout · writes the server's own settings (bridge/config.local.json) · installs + enables the systemd
# service · makes the deploy key the Forgejo workflow uses (locked to ONE command: the deploy itself) · with --with-nginx
# installs the nginx site · prints exactly what is left for people (Gee's Claude login, the Forgejo secrets, the data).
# It never touches account or chat data: those arrive with deploy/restore-data.sh, copied as they are.
set -eu
BASE=/srv/viberoller; HOST=viberoller.git.unityailab.com; REPO=https://git.unityailab.com/GFourteen/VibeRoller.git; SVC=viberoller; NGINX=0
while [ $# -gt 0 ]; do case "$1" in
  --with-nginx) NGINX=1;; --dir) BASE=$2; shift;; --host) HOST=$2; shift;; --repo) REPO=$2; shift;; --user) SVC=$2; shift;;
  *) echo "unknown option $1"; exit 1;; esac; shift; done
[ "$(id -u)" = 0 ] || { echo "Run as root (sudo)."; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)
APP=$BASE/app; HOME_DIR=$BASE/home
say() { printf '\n== %s\n' "$*"; }
as_svc() { runuser -u "$SVC" -- env HOME="$HOME_DIR" "$@"; }

say "Tools"
command -v git >/dev/null || { echo "git is required (apt install git)."; exit 1; }
command -v node >/dev/null || { echo "Node.js 22 or newer is required for every user (e.g. the NodeSource setup script)."; exit 1; }
NODE_MAJOR=$(node -p 'process.versions.node.split(".")[0]')
[ "$NODE_MAJOR" -ge 22 ] || { echo "Node $(node --version) found; 22 or newer is required."; exit 1; }
command -v claude >/dev/null && echo "claude: $(claude --version 2>/dev/null || echo present)" || echo "claude is missing: npm install -g @anthropic-ai/claude-code (Work runs need it; everything else works without it)"

say "Service user + folders ($SVC, $BASE)"
id "$SVC" >/dev/null 2>&1 || useradd --system --home-dir "$HOME_DIR" --create-home --shell /bin/sh "$SVC"
usermod -s /bin/sh "$SVC" # the deploy key needs a shell; its forced command below is all that key can run
mkdir -p "$APP" "$BASE/worktrees" "$HOME_DIR/.ssh"
chown -R "$SVC:$SVC" "$BASE"; chmod 750 "$BASE"; chmod 700 "$HOME_DIR/.ssh"

say "Live checkout ($APP)"
if [ -d "$APP/.git" ]; then as_svc git -C "$APP" fetch -q origin main && as_svc git -C "$APP" merge -q --ff-only origin/main && echo "fast-forwarded to $(git -C "$APP" rev-parse --short HEAD)"
else as_svc git clone -q "$REPO" "$APP" && echo "cloned $(git -C "$APP" rev-parse --short HEAD)"; fi
as_svc git -C "$APP" config user.name "VibeRoller hub"; as_svc git -C "$APP" config user.email "viberoller@$HOST"

say "Server settings (bridge/config.local.json)"
if [ ! -f "$APP/bridge/config.local.json" ]; then
  sed -e "s#https://viberoller.git.unityailab.com#https://$HOST#" -e "s#/srv/viberoller/worktrees#$BASE/worktrees#" "$APP/deploy/config.local.example.json" > "$APP/bridge/config.local.json"
  chown "$SVC:$SVC" "$APP/bridge/config.local.json"; chmod 640 "$APP/bridge/config.local.json"; echo "written (publicUrl https://$HOST, no tunnel, behind nginx)"
else echo "kept as it is"; fi

say "systemd service (viberoller-hub)"
sed -e "s#/srv/viberoller#$BASE#g" -e "s#User=viberoller#User=$SVC#" -e "s#Group=viberoller#Group=$SVC#" "$APP/deploy/viberoller-hub.service" > /etc/systemd/system/viberoller-hub.service
systemctl daemon-reload; systemctl enable -q viberoller-hub; systemctl restart viberoller-hub
sleep 3; PORT=$(sed -n 's/.*"port"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' "$APP/bridge/config.local.json" | head -n 1); PORT=${PORT:-7777}
curl -fsS -m 5 "http://127.0.0.1:$PORT/api/status" >/dev/null && echo "the hub answers on 127.0.0.1:$PORT" || echo "the hub did not answer yet: journalctl -u viberoller-hub -n 50"

say "Deploy key for the Forgejo workflow (one command only)"
KEY=$HOME_DIR/.ssh/deploy
[ -f "$KEY" ] || as_svc ssh-keygen -q -t ed25519 -N '' -C "viberoller-deploy" -f "$KEY"
DEPLOY_CMD="cd $APP && git fetch -q origin main && git merge -q --ff-only origin/main && for f in bridge/*.js; do node --check \$f; done && node bridge/restart.js && echo deployed \$(git rev-parse --short HEAD)"
grep -q 'viberoller-deploy' "$HOME_DIR/.ssh/authorized_keys" 2>/dev/null || printf 'command="%s",no-port-forwarding,no-agent-forwarding,no-pty,no-X11-forwarding %s\n' "$DEPLOY_CMD" "$(cat "$KEY.pub")" >> "$HOME_DIR/.ssh/authorized_keys"
chown "$SVC:$SVC" "$HOME_DIR/.ssh/authorized_keys" "$KEY" "$KEY.pub"; chmod 600 "$HOME_DIR/.ssh/authorized_keys" "$KEY"
base64 -w0 "$KEY" > "$BASE/deploy-key.b64"; chmod 600 "$BASE/deploy-key.b64"
echo "private key (base64, root only): $BASE/deploy-key.b64"

if [ "$NGINX" = 1 ]; then
  say "nginx site ($HOST)"
  command -v nginx >/dev/null || { echo "nginx is not installed"; exit 1; }
  sed -e "s#viberoller.git.unityailab.com#$HOST#g" -e "s#127.0.0.1:7777#127.0.0.1:$PORT#" "$APP/deploy/nginx-viberoller.conf" > /etc/nginx/sites-available/viberoller.conf
  ln -sf ../sites-available/viberoller.conf /etc/nginx/sites-enabled/viberoller.conf
  if nginx -t 2>/dev/null; then systemctl reload nginx; echo "installed and reloaded"; else echo "nginx -t failed: set the two ssl_ lines in /etc/nginx/sites-available/viberoller.conf to the lab wildcard certificate, then: nginx -t && systemctl reload nginx"; fi
else echo; echo "(nginx not touched: run again with --with-nginx, or install deploy/nginx-viberoller.conf by hand)"; fi

say "Left for people"
cat <<EOF
1. Gee's Claude login on the server (Gee, once; Work runs use it):
     runuser -u $SVC -- env HOME=$HOME_DIR claude login
2. Forgejo → GFourteen/VibeRoller → Settings → Actions:
     secrets   VIBEROLLER_SSH_KEY  = the contents of $BASE/deploy-key.b64
               VIBEROLLER_SSH_HOST = this server's host name     VIBEROLLER_SSH_USER = $SVC
     variables VIBEROLLER_DIR = $APP                              VIBEROLLER_DEPLOY = on   (the switch)
3. The data, only with Gee's approval (deploy/pack-data.cmd on Gee's PC → copy the archive here → sudo sh deploy/restore-data.sh <archive>).
4. Sign in at https://$HOST, check every account, then stop the hub on Gee's PC and start his AI node there.
EOF
