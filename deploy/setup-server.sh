#!/usr/bin/env sh
# VibeRoller: make the lab server the hub's home, WITHOUT root. Run as the account that owns the hub's work area (gee,
# mills or sponge: all three are in the VibeRoller and sharedwork groups), once; safe to run again (every step checks first).
#
#   sh deploy/setup-server.sh [--base /srv/apps/VibeRoller] [--projects /srv/apps/sharedwork]
#                             [--host portal.unityailab.com] [--repo https://git.unityailab.com/GFourteen/VibeRoller.git]
#
# It: puts a user-local Node 22 under <base>/node when the system's is older · installs Claude Code under <base>/tools ·
# clones (or fast-forwards) the live checkout <base>/app · writes the server's own settings (bridge/config.local.json,
# with a fresh node secret) · a git identity for the hub · the systemd USER services: the hub (viberoller-hub) and the
# auto-deploy timer (viberoller-autodeploy: every push to main goes live by itself) · the two nginx site files, ready in
# <base>/deploy-ready for the one root step · and prints what is left for people. It never touches account or chat data:
# those arrive with deploy/restore-data.sh, exactly as they are.
#
# Root is needed exactly three times, by whoever has it (printed at the end): loginctl enable-linger <user> (the hub runs
# from boot), the nginx sites, and the DNS record + certificate for portal.unityailab.com.
set -eu
BASE=/srv/apps/VibeRoller; PROJECTS=/srv/apps/sharedwork; HOST=portal.unityailab.com; REPO=https://git.unityailab.com/GFourteen/VibeRoller.git
while [ $# -gt 0 ]; do case "$1" in
  --base) BASE=$2; shift;; --projects) PROJECTS=$2; shift;; --host) HOST=$2; shift;; --repo) REPO=$2; shift;;
  *) echo "unknown option $1"; exit 1;; esac; shift; done
[ "$(id -u)" = 0 ] && { echo "Run as the hub's account (not root): the work area belongs to the team's group."; exit 1; }
APP=$BASE/app; HOME_DIR=$BASE/home; UNITS=$HOME/.config/systemd/user; ME=$(id -un)
say() { printf '\n== %s\n' "$*"; }
umask 0007 # group-writable: the whole team may read and repair the work area

say "Folders ($BASE, projects in $PROJECTS)"
[ -d "$BASE" ] && [ -w "$BASE" ] || { echo "$BASE must exist and be writable by $ME (root: mkdir, chgrp VibeRoller, chmod 2770)."; exit 1; }
[ -d "$PROJECTS" ] && [ -w "$PROJECTS" ] || { echo "$PROJECTS must exist and be writable by $ME (root: mkdir, chgrp sharedwork, chmod 2770)."; exit 1; }
mkdir -p "$HOME_DIR/.claude" "$BASE/tools" "$BASE/deploy-ready" "$PROJECTS/VibeRoller" "$UNITS"

say "Tools"
command -v git >/dev/null || { echo "git is required."; exit 1; }
NODE_BIN=$BASE/node/current/bin
if [ ! -x "$NODE_BIN/node" ]; then
  SYS=$(command -v node >/dev/null && node -p 'process.versions.node.split(".")[0]' || echo 0)
  if [ "$SYS" -ge 22 ]; then mkdir -p "$BASE/node/current/bin"; ln -sf "$(command -v node)" "$NODE_BIN/node"; ln -sf "$(command -v npm)" "$NODE_BIN/npm"; echo "node: system $(node --version)"
  else
    echo "system node is too old ($(command -v node >/dev/null && node --version || echo none)); fetching Node 22 for this account"
    command -v curl >/dev/null || { echo "curl is required to fetch Node."; exit 1; }
    ARCH=$(uname -m | sed 's/x86_64/x64/; s/aarch64/arm64/'); V=$(curl -fsSL https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt | sed -n 's/.*node-\(v22[^-]*\)-linux-'"$ARCH"'\.tar\.xz$/\1/p' | head -n 1)
    [ -n "$V" ] || { echo "could not find the latest Node 22 for $ARCH"; exit 1; }
    mkdir -p "$BASE/node"; curl -fsSL "https://nodejs.org/dist/$V/node-$V-linux-$ARCH.tar.xz" | tar -xJ -C "$BASE/node"
    ln -sfn "$BASE/node/node-$V-linux-$ARCH" "$BASE/node/current"; echo "node: $V under $BASE/node"
  fi
else echo "node: $("$NODE_BIN/node" --version) under $BASE/node"; fi
export PATH="$NODE_BIN:$BASE/tools/bin:$PATH"
if [ ! -x "$BASE/tools/bin/claude" ]; then
  echo "installing Claude Code under $BASE/tools"; HOME=$HOME_DIR npm_config_prefix="$BASE/tools" "$NODE_BIN/npm" install -g @anthropic-ai/claude-code >/dev/null 2>&1 && echo "claude: $("$BASE/tools/bin/claude" --version 2>/dev/null || echo installed)" || echo "claude could not be installed (Work runs need it; everything else works without it): npm_config_prefix=$BASE/tools npm install -g @anthropic-ai/claude-code"
else echo "claude: $("$BASE/tools/bin/claude" --version 2>/dev/null || echo present)"; fi

say "Live checkout ($APP)"
if [ -d "$APP/.git" ]; then git -C "$APP" fetch -q origin main && git -C "$APP" merge -q --ff-only origin/main && echo "fast-forwarded to $(git -C "$APP" rev-parse --short HEAD)"
else git clone -q "$REPO" "$APP" && echo "cloned $(git -C "$APP" rev-parse --short HEAD)"; fi
git -C "$APP" config core.sharedRepository group
[ -f "$HOME_DIR/.gitconfig" ] || HOME=$HOME_DIR git config --global user.name "VibeRoller hub" && HOME=$HOME_DIR git config --global user.email "viberoller@$HOST"

say "Server settings (bridge/config.local.json)"
if [ ! -f "$APP/bridge/config.local.json" ]; then
  SECRET=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
  sed -e "s#https://portal.unityailab.com#https://$HOST#" -e "s#/srv/apps/sharedwork#$PROJECTS#g" -e "s#__NODE_SECRET__#$SECRET#" "$APP/deploy/config.local.example.json" > "$APP/bridge/config.local.json"
  chmod 660 "$APP/bridge/config.local.json"; echo "written (publicUrl https://$HOST, no tunnel, behind nginx, projects in $PROJECTS); the AI node secret for gee-pc is inside"
else echo "kept as it is"; fi

say "systemd user services (viberoller-hub, viberoller-autodeploy)"
for u in viberoller-hub.service viberoller-autodeploy.service viberoller-autodeploy.timer; do sed -e "s#__BASE__#$BASE#g" "$APP/deploy/$u" > "$UNITS/$u"; done
chmod +x "$APP/deploy/autodeploy.sh" 2>/dev/null || true
systemctl --user daemon-reload
systemctl --user enable -q viberoller-hub viberoller-autodeploy.timer
systemctl --user restart viberoller-hub; systemctl --user start viberoller-autodeploy.timer
sleep 3; PORT=$(sed -n 's/.*"port"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' "$APP/bridge/config.local.json" | head -n 1); PORT=${PORT:-7777}
curl -fsS -m 5 "http://127.0.0.1:$PORT/api/status" >/dev/null && echo "the hub answers on 127.0.0.1:$PORT" || echo "the hub did not answer yet: journalctl --user -u viberoller-hub -n 50"
loginctl show-user "$ME" 2>/dev/null | grep -q 'Linger=yes' && echo "linger: on ($ME's services run from boot)" || echo "linger: OFF: root must run  loginctl enable-linger $ME  or the hub stops when $ME's last session ends"

say "nginx site files ($BASE/deploy-ready, for the root step)"
sed -e "s#127.0.0.1:7777#127.0.0.1:$PORT#" "$APP/deploy/nginx-portal.conf" > "$BASE/deploy-ready/portal.unityailab.com"
sed -e "s#127.0.0.1:7777#127.0.0.1:$PORT#" "$APP/deploy/nginx-viberoller.conf" > "$BASE/deploy-ready/viberoller.git.unityailab.com"
echo "ready: portal.unityailab.com (the private front door) and viberoller.git.unityailab.com (the plain name)"

say "Left for people"
cat <<EOF
1. Root, once (whoever has sudo on the server):
     loginctl enable-linger $ME
     cp $BASE/deploy-ready/portal.unityailab.com $BASE/deploy-ready/viberoller.git.unityailab.com /etc/nginx/sites-available/
     ln -s ../sites-available/portal.unityailab.com /etc/nginx/sites-enabled/; ln -s ../sites-available/viberoller.git.unityailab.com /etc/nginx/sites-enabled/
     nginx -t && systemctl reload nginx
   and for the short name: a DNS A record portal.unityailab.com -> this server, then  certbot --nginx -d portal.unityailab.com
   (portal.git.unityailab.com works at once: the lab's wildcard certificate covers it).
2. Gee's Claude login on the server (Gee, once; Work runs use it):
     HOME=$HOME_DIR CLAUDE_CONFIG_DIR=$HOME_DIR/.claude $BASE/tools/bin/claude login
3. The data, with the hub on Gee's PC stopped: on his PC  deploy\\pack-data.cmd ovh  (packs + copies), then here:
     sh $APP/deploy/restore-data.sh $BASE/viberoller-data-<date>.tar.gz
4. Gee's PC becomes an AI host: Download → Vibe Roller AI Host (Windows), ai-node.json with the gee-pc secret from
   $APP/bridge/config.local.json, build.cmd, say Y to the service.
EOF
