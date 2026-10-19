#!/usr/bin/env sh
# VibeRoller auto-deploy: every push to main on Forgejo goes live on the server by itself, without a deploy key, a runner
# or any secret. A systemd user timer (deploy/viberoller-autodeploy.timer, installed by deploy/setup-server.sh) runs this
# every minute as the hub's account:
#   fetch main · nothing new → done · else fast-forward the live checkout (never anything but a fast-forward: a checkout
#   someone changed by hand stops the deploy instead of being overwritten) · check every hub script parses · ask the hub
#   for a graceful restart (bridge/.restart: same address, nobody signed out, it waits for any reply in progress).
# It never touches the hub's data: accounts, chat, board, notes and pictures are gitignored, and a fast-forward never
# deletes ignored files. Log: __BASE__/autodeploy.log (kept short).
#   sh deploy/autodeploy.sh [app dir]        by hand; APP defaults to the folder this script lives in
set -eu
APP=${1:-$(cd "$(dirname "$0")/.." && pwd)}
LOG=$(cd "$APP/.." && pwd)/autodeploy.log
say() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" | tee -a "$LOG"; }
[ -s "$LOG" ] && [ "$(wc -l < "$LOG")" -gt 500 ] && tail -n 200 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"
cd "$APP"
git fetch -q origin main || { say "fetch failed"; exit 1; }
LOCAL=$(git rev-parse HEAD); REMOTE=$(git rev-parse origin/main)
[ "$LOCAL" = "$REMOTE" ] && exit 0
[ -z "$(git status --porcelain)" ] || { say "live checkout has uncommitted changes; not deploying $(echo "$REMOTE" | cut -c1-7)"; exit 1; }
git merge -q --ff-only origin/main || { say "main moved in a way that is not a fast-forward; not deploying"; exit 1; }
for f in bridge/*.js; do node --check "$f" || { say "node --check failed for $f after $(echo "$REMOTE" | cut -c1-7); the hub keeps running the old code until this is fixed"; exit 1; }; done
date -u +%Y-%m-%dT%H:%M:%SZ > bridge/.restart
say "deployed $(echo "$LOCAL" | cut -c1-7) → $(echo "$REMOTE" | cut -c1-7) (VibeRoller $(sed -n 's/.*"project"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' versions.json | head -n 1)); graceful restart requested"
