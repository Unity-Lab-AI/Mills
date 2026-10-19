#!/usr/bin/env sh
# VibeRoller auto-deploy: every push to main on Forgejo goes live on the server by itself, without a deploy key, a runner
# or any secret. A systemd user timer (deploy/viberoller-autodeploy.timer, installed by deploy/setup-server.sh) runs this
# every minute as the hub's account.
#
# Deploy: fetch main · nothing new (same commit, already inside the checkout, or the commit that was rolled back) → done ·
#   snapshot the hub's data (__BASE__/snapshots, the last 10) · fast-forward the live checkout (never anything but a
#   fast-forward: a checkout someone changed by hand stops the deploy instead of being overwritten) · check every hub
#   script parses · ask the hub for a graceful restart (bridge/.restart: same address, nobody signed out, it waits for
#   any reply in progress) · remember the deploy as pending.
# Verify (the next runs): once the hub has restarted, it must answer /api/status with the new commit within 2 minutes
#   (or at all within 3 runs, if it never comes back). Then that commit is the last good one. If not, the live checkout
#   goes back to the last good commit, the hub restarts on it, the team chat says so (bridge/.rolled-back), and that
#   commit is not deployed again until main moves on.
# It never touches the hub's data: accounts, chat, board, notes and pictures are gitignored, and neither a fast-forward
# nor going back deletes ignored files. Log: __BASE__/autodeploy.log (kept short); state: __BASE__/deploy-state/.
#   sh deploy/autodeploy.sh [app dir]        by hand; APP defaults to the folder this script lives in
set -eu
APP=${1:-$(cd "$(dirname "$0")/.." && pwd)}
BASE=$(cd "$APP/.." && pwd)
LOG=$BASE/autodeploy.log
ST=$BASE/deploy-state
mkdir -p "$ST"
say() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" | tee -a "$LOG"; }
[ -s "$LOG" ] && [ "$(wc -l < "$LOG")" -gt 500 ] && tail -n 200 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"
cd "$APP"
short() { echo "$1" | cut -c1-7; }
version() { sed -n 's/.*"project"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' versions.json | head -n 1; }
PORT=$(sed -n 's/.*"port"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' bridge/config.local.json bridge/config.json 2>/dev/null | head -n 1); PORT=${PORT:-7777}
running() { curl -fsS -m 5 "http://127.0.0.1:$PORT/api/status" 2>/dev/null | sed -n 's/.*"commit":"\([0-9a-f]*\)".*/\1/p'; }
answers() { curl -fsS -m 5 "http://127.0.0.1:$PORT/api/status" >/dev/null 2>&1; }
NOW=$(date +%s)

# ---- verify the last deploy ----
if [ -f "$ST/pending" ]; then
  read -r NEW OLD SINCE RESTARTED FAILS < "$ST/pending" || true
  RESTARTED=${RESTARTED:-0}; FAILS=${FAILS:-0}
  if [ "$(running)" = "$NEW" ]; then
    echo "$NEW" > "$ST/last-good"; rm -f "$ST/pending"
    say "verified $(short "$NEW") (VibeRoller $(version)): the hub is up on it; it is now the last good version"
    exit 0
  fi
  answers && FAILS=0 || FAILS=$((FAILS + 1))
  if [ -f bridge/.restart ] && [ "$FAILS" -lt 3 ]; then # the hub has not restarted yet: it waits for replies in progress
    [ $((NOW - SINCE)) -gt 21600 ] && say "still waiting for the hub to restart onto $(short "$NEW") (a reply has been in progress for hours?)"
    echo "$NEW $OLD $SINCE 0 $FAILS" > "$ST/pending"; exit 0
  fi
  [ "$RESTARTED" = 0 ] && RESTARTED=$NOW
  if [ $((NOW - RESTARTED)) -lt 120 ] && [ "$FAILS" -lt 3 ]; then echo "$NEW $OLD $SINCE $RESTARTED $FAILS" > "$ST/pending"; exit 0; fi
  # it did not come up on the new commit: back to the last good one
  GOOD=$(cat "$ST/last-good" 2>/dev/null || echo "$OLD")
  echo "$NEW" > "$ST/bad"; rm -f "$ST/pending"
  git reset -q --hard "$GOOD"
  date -u +%Y-%m-%dT%H:%M:%SZ > bridge/.restart
  printf 'Version %s did not come up within 2 minutes of its restart, so the hub went back to %s (VibeRoller %s), the last version that ran fine. %s stays out until main moves on.\n' "$(short "$NEW")" "$(short "$GOOD")" "$(version)" "$(short "$NEW")" > bridge/.rolled-back
  say "ROLLED BACK: $(short "$NEW") did not come up (hub answered: $(answers && echo "yes, on $(short "$(running)")" || echo no)); back on $(short "$GOOD") (VibeRoller $(version)); $(short "$NEW") is skipped until main moves on"
  exit 0
fi

# ---- deploy what is new ----
git fetch -q origin main || { say "fetch failed"; exit 1; }
LOCAL=$(git rev-parse HEAD); REMOTE=$(git rev-parse origin/main)
[ "$LOCAL" = "$REMOTE" ] && exit 0
# origin/main already inside the checkout (commits made on the server that are not on Forgejo yet): nothing new to deploy.
# Without this the hub was restarted every minute for nothing (production bug 2026-10-19: every AI host lost its line).
git merge-base --is-ancestor "$REMOTE" "$LOCAL" && exit 0
# the commit that was rolled back stays out until main moves on (a fix gets pushed)
[ "$(cat "$ST/bad" 2>/dev/null)" = "$REMOTE" ] && exit 0
[ -z "$(git status --porcelain)" ] || { say "live checkout has uncommitted changes; not deploying $(short "$REMOTE")"; exit 1; }
# the version running now is good if the hub answers on it (the first deploy with this script, or after a by-hand change)
[ -f "$ST/last-good" ] || { [ "$(running)" = "$LOCAL" ] && echo "$LOCAL" > "$ST/last-good"; } || true
# a snapshot of the data before anything changes (the same files deploy/pack-data.sh packs), the last 10 kept
SNAP=$BASE/snapshots; mkdir -p "$SNAP"
LIST=''
for p in bridge/auth.json bridge/state.json bridge/hub-state.json bridge/usage.json bridge/assistant.json bridge/providers.json bridge/forgejo.json bridge/forgejo-users.json bridge/server-owners.json bridge/ai-hosts.json bridge/config.local.json bridge/profiles bridge/profiles-deleted Chat Archive Board Notes Avatars Images Api; do
  [ -e "$p" ] && LIST="$LIST $p"
done
if [ -n "$LIST" ]; then
  # shellcheck disable=SC2086
  (umask 077; tar -czf "$SNAP/data-$(date +%Y%m%d-%H%M%S)-$(short "$LOCAL").tar.gz" --exclude='*.tmp' --exclude='*.log' $LIST 2>/dev/null) || say "data snapshot failed (deploying anyway; the data itself is never touched)"
fi
ls -1t "$SNAP"/data-*.tar.gz 2>/dev/null | tail -n +11 | while read -r old; do rm -f "$old"; done
git merge -q --ff-only origin/main || { say "main moved in a way that is not a fast-forward; not deploying"; exit 1; }
for f in bridge/*.js; do node --check "$f" || { say "node --check failed for $f in $(short "$REMOTE"); staying on $(short "$LOCAL") and skipping $(short "$REMOTE") until main moves on"; git reset -q --hard "$LOCAL"; echo "$REMOTE" > "$ST/bad"; exit 1; }; done
date -u +%Y-%m-%dT%H:%M:%SZ > bridge/.restart
echo "$REMOTE $LOCAL $NOW 0 0" > "$ST/pending"
say "deployed $(short "$LOCAL") → $(short "$REMOTE") (VibeRoller $(version)); graceful restart requested; checking it comes up"
