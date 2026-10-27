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
# The dev hub (LAWS.md, dev hub law; team note plans/dev-hub.md) runs the same script with VR_DEPLOY_BRANCH=develop and
# VR_INSTANCE=dev (deploy/viberoller-dev-autodeploy.service): it follows develop instead of main, under its own base.
set -eu
BRANCH=${VR_DEPLOY_BRANCH:-main}
INSTANCE=${VR_INSTANCE:-live}
NATIVE_UNIT=viberoller-native-build; [ "$INSTANCE" = live ] || NATIVE_UNIT="viberoller-$INSTANCE-native-build"
APP=${1:-$(cd "$(dirname "$0")/.." && pwd)}
BASE=$(cd "$APP/.." && pwd)
LOG=$BASE/autodeploy.log
ST=$BASE/deploy-state
mkdir -p "$ST"
# Run from a copy: a deploy rewrites this very file, and sh reads a script as it goes (the 3.2.0 deploy ran half old,
# half new and skipped its log line).
if [ "${VR_AUTODEPLOY_COPY:-}" != 1 ]; then
  cp "$APP/deploy/autodeploy.sh" "$ST/autodeploy.run.sh"
  VR_AUTODEPLOY_COPY=1 exec sh "$ST/autodeploy.run.sh" "$APP"
fi
say() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" | tee -a "$LOG"; }
[ -s "$LOG" ] && [ "$(wc -l < "$LOG")" -gt 500 ] && tail -n 200 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"
cd "$APP"
short() { echo "$1" | cut -c1-7; }
label() { [ "$INSTANCE" = live ] && echo "VibeRoller $(version)" || echo "VibeRoller $(version), $INSTANCE hub"; }
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
    # the good versions are recorded newest first: last-good, last-good-2 (the one before). The Rust builds of the last two
    # good versions are kept besides the current one (native/keep-builds.sh), so the fallback chain is two deep (T-157)
    [ -f "$ST/last-good" ] && [ "$(cat "$ST/last-good")" != "$NEW" ] && cp "$ST/last-good" "$ST/last-good-2"
    echo "$NEW" > "$ST/last-good"; rm -f "$ST/pending"
    # the kept builds (FR3): a verified version's build is a good one (newest first; native/keep-builds.sh keeps the 2 newest)
    for T in x86_64-unknown-linux-gnu x86_64-pc-windows-gnu; do
      G=native/dist/$T/good
      if [ -d "native/dist/$T/$NEW" ]; then { echo "$NEW"; [ -f "$G" ] && grep -vx "$NEW" "$G"; } > "$G.tmp" 2>/dev/null; mv "$G.tmp" "$G"; fi
    done
    say "verified $(short "$NEW") ($(label)): the hub is up on it; it is now the last good version"
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
  # the kept builds (FR3): the last good version's build goes back in service with its checkout (when it was kept)
  for T in x86_64-unknown-linux-gnu x86_64-pc-windows-gnu; do
    [ -d "native/dist/$T/$GOOD" ] && sh native/use-build.sh "native/dist/$T" "$GOOD" >> "$LOG" 2>&1 && say "the native build of $(short "$GOOD") is back in service for $T"
  done
  date -u +%Y-%m-%dT%H:%M:%SZ > bridge/.restart
  printf 'Version %s did not come up within 2 minutes of its restart, so the hub went back to %s (VibeRoller %s), the last version that ran fine. %s stays out until $BRANCH moves on.\n' "$(short "$NEW")" "$(short "$GOOD")" "$(version)" "$(short "$NEW")" > bridge/.rolled-back
  say "ROLLED BACK: $(short "$NEW") did not come up (hub answered: $(answers && echo "yes, on $(short "$(running)")" || echo no)); back on $(short "$GOOD") ($(label)); $(short "$NEW") is skipped until $BRANCH moves on"
  exit 0
fi

# ---- the native programs (pre-built binaries for the Downloads and the updates): rebuilt in the background whenever the
# running version's native/ differs from what native/dist was built from (or nothing is built yet). It never holds a
# deploy up, a failed build changes nothing (the last build stays), and only one runs at a time. Log: __BASE__/native-build.log
if [ -f native/build.sh ] && [ ! -f "$ST/pending" ]; then
  BUILT=$(sed -n 's/.*"commit":"\([0-9a-f]*\)".*/\1/p' native/dist/x86_64-unknown-linux-gnu/BUILD.json 2>/dev/null | head -n 1)
  NEEDS=0
  if [ -z "$BUILT" ]; then NEEDS=1; elif ! git cat-file -e "$BUILT^{commit}" 2>/dev/null; then NEEDS=1; elif ! git diff --quiet "$BUILT" HEAD -- native/ 2>/dev/null; then NEEDS=1; fi
  # a build of this very commit that failed its trial is not tried again until main moves on
  [ "$NEEDS" = 1 ] && [ -d "native/dist/x86_64-unknown-linux-gnu/$(git rev-parse HEAD)" ] && [ "$(cat native/dist/x86_64-unknown-linux-gnu/current 2>/dev/null)" != "$(git rev-parse HEAD)" ] && NEEDS=0
  LOCK=$ST/native-build.pid
  # The build is staged under native/dist/<target>/<commit>/ (VR_STAGE_ONLY=1), tried on a side port against a copy of
  # the data (deploy/trial.sh, FR5), marked good (native/mark-good.sh: so keep-builds keeps it as a fallback once a newer
  # build comes, 5.13.3) and only then put in service (native/use-build.sh, FR3; the Windows build with it).
  # A finished build asks the hub for a graceful restart (it waits for nobody being busy), so the new Rust hub (vr-hub)
  # runs too, not just the new JavaScript; the AI hosts take their new build from it by themselves. A failed trial keeps
  # the old build and says so in the team chat (bridge/.announce/, posted by the hub).
  BC=$(git rev-parse HEAD); LT=x86_64-unknown-linux-gnu; WT=x86_64-pc-windows-gnu
  TRIAL_FAIL="mkdir -p '$APP/bridge/.announce' && printf '**A new native build was NOT put in service.** The build of %s failed its trial (deploy/trial.sh on a side port against a copy of the data); the build in service stays. See native-build.log on the server.\n' '$(short "$BC")' > '$APP/bridge/.announce/trial-failed-\$(date +%s).txt'"
  BUILD_CMD="export VR_COMMIT='$BC' VR_STAGE_ONLY=1; nice -n 10 sh '$APP/native/build.sh' > '$BASE/native-build.log' 2>&1 || { echo \"\$(date '+%Y-%m-%d %H:%M:%S') native build FAILED (the last build stays; see native-build.log)\" >> '$LOG'; exit 1; }; if sh '$APP/deploy/trial.sh' '$APP/native/dist/$LT/$BC/vr-hub' '$APP' >> '$BASE/native-build.log' 2>&1; then sh '$APP/native/mark-good.sh' '$APP/native/dist/$LT' '$BC' >> '$BASE/native-build.log' 2>&1; [ -d '$APP/native/dist/$WT/$BC' ] && sh '$APP/native/mark-good.sh' '$APP/native/dist/$WT' '$BC' >> '$BASE/native-build.log' 2>&1; sh '$APP/native/use-build.sh' '$APP/native/dist/$LT' '$BC' >> '$BASE/native-build.log' 2>&1 && { [ -d '$APP/native/dist/$WT/$BC' ] && sh '$APP/native/use-build.sh' '$APP/native/dist/$WT' '$BC' >> '$BASE/native-build.log' 2>&1; true; } && date -u +%Y-%m-%dT%H:%M:%SZ > '$APP/bridge/.restart' && echo \"\$(date '+%Y-%m-%d %H:%M:%S') native build done and tried; graceful restart requested (the new Rust hub)\" >> '$LOG'; else echo \"\$(date '+%Y-%m-%d %H:%M:%S') native build of $(short "$BC") FAILED ITS TRIAL (the build in service stays; see native-build.log)\" >> '$LOG'; $TRIAL_FAIL; fi"
  busy=0
  if [ -n "${INVOCATION_ID:-}" ] && command -v systemd-run >/dev/null; then systemctl --user is-active -q "$NATIVE_UNIT" 2>/dev/null && busy=1
  else [ -f "$LOCK" ] && kill -0 "$(cat "$LOCK")" 2>/dev/null && busy=1; fi
  if [ "$NEEDS" = 1 ] && [ "$busy" = 0 ]; then
    say "building the native programs for $(short "$(git rev-parse HEAD)") in the background (native-build.log)"
    # under the systemd timer a background child would end with this (one-shot) run: the build gets a unit of its own
    if [ -n "${INVOCATION_ID:-}" ] && command -v systemd-run >/dev/null; then systemd-run --user --quiet --collect --unit "$NATIVE_UNIT" sh -c "$BUILD_CMD" || say "could not start the native build"
    else (sh -c "$BUILD_CMD"; rm -f "$LOCK") & echo $! > "$LOCK"; fi
  fi
fi

# ---- deploy what is new ----
git fetch -q origin "$BRANCH" || { say "fetch of $BRANCH failed"; exit 1; }
LOCAL=$(git rev-parse HEAD); REMOTE=$(git rev-parse "origin/$BRANCH")
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
for p in bridge/auth.json bridge/state.json bridge/work.json bridge/work-current.json bridge/api-keys.json bridge/hub-state.json bridge/front-sessions.json bridge/device-locks.json bridge/release-key.json bridge/hub-signing.key bridge/usage.json bridge/assistant.json bridge/providers.json bridge/forgejo.json bridge/forgejo-users.json bridge/server-owners.json bridge/ai-hosts.json bridge/gateway bridge/config.local.json bridge/profiles bridge/profiles-deleted Chat Archive Board Notes Avatars Images Api; do
  [ -e "$p" ] && LIST="$LIST $p"
done
if [ -n "$LIST" ]; then
  # shellcheck disable=SC2086
  (umask 077; tar -czf "$SNAP/data-$(date +%Y%m%d-%H%M%S)-$(short "$LOCAL").tar.gz" --exclude='*.tmp' --exclude='*.log' $LIST 2>/dev/null) || say "data snapshot failed (deploying anyway; the data itself is never touched)"
fi
ls -1t "$SNAP"/data-*.tar.gz 2>/dev/null | tail -n +11 | while read -r old; do rm -f "$old"; done
git merge -q --ff-only "origin/$BRANCH" || { say "$BRANCH moved in a way that is not a fast-forward; not deploying"; exit 1; }
for f in bridge/*.js; do node --check "$f" || { say "node --check failed for $f in $(short "$REMOTE"); staying on $(short "$LOCAL") and skipping $(short "$REMOTE") until $BRANCH moves on"; git reset -q --hard "$LOCAL"; echo "$REMOTE" > "$ST/bad"; exit 1; }; done
date -u +%Y-%m-%dT%H:%M:%SZ > bridge/.restart
echo "$REMOTE $LOCAL $NOW 0 0" > "$ST/pending"
say "deployed $(short "$LOCAL") → $(short "$REMOTE") ($(label)); graceful restart requested; checking it comes up"
