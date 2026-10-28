#!/bin/sh
# The dev hub (LAWS.md, dev hub law; team note plans/dev-hub.md): a second hub on this server that runs Forgejo's
# develop on 127.0.0.1:7778 (nginx: dev.portal.unityailab.com), with a full copy of live's data. No root needed: it runs
# as the hub's account, with user services like the live hub's. The nginx site, the DNS record and the certificate are
# root's (deploy/nginx-dev-portal.conf says how).
#
#   sh deploy/setup-dev.sh                 install (or repair) the dev hub and start it
#   sh deploy/setup-dev.sh --no-start      install without starting the services (to look first)
#   sh deploy/setup-dev.sh pair            switch the bridge on: both hubs sync with each other (both need a build
#                                          that has the bridge; asks both for a graceful restart)
# Environment: VR_LIVE_BASE (default /srv/apps/VibeRoller), VR_DEV_BASE (default $VR_LIVE_BASE/dev),
#   VR_DEV_URL (default https://dev.portal.unityailab.com), VR_DEV_UNITS (default the account home .config/systemd/user; "" = no services).
#
# What it does, once each (running it again repairs and changes nothing that is there):
#   1. the dev checkout: a clone of live's origin (Forgejo) on develop, at $VR_DEV_BASE/app;
#   2. its data: a copy of live's data (the same files the auto-deploy snapshots), never the sign-in sessions, device
#      unlocks or AI-host sessions (each hub signs in on its own); copied once, then the bridge keeps the two the same;
#   3. its settings: live's bridge/config.local.json with port 7778, its own back port, instance "dev", its own
#      address, and no AI hosts (they belong to live until one opts in);
#   4. its native builds: live's current ones, so it can start before its own first (clean) build;
#   5. the user services viberoller-dev-hub + the dev auto-deploy timer (following develop), started, and the commit it
#      came up on recorded as its last good one (worktree.js live reads it).
# It never writes anything of the live hub's, except `pair`, which adds the bridge setting to live's config.
set -eu
LIVE=${VR_LIVE_BASE:-/srv/apps/VibeRoller}
DEV=${VR_DEV_BASE:-$LIVE/dev}
URL=${VR_DEV_URL:-https://dev.portal.unityailab.com}
# the account's own home, not $HOME: a Work run's HOME is its own folder, where systemd --user never looks
UHOME=$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f6); UHOME=${UHOME:-$HOME}
UNITS=${VR_DEV_UNITS-$UHOME/.config/systemd/user}
LAPP=$LIVE/app; DAPP=$DEV/app
NODE=$LIVE/node/current/bin/node; [ -x "$NODE" ] || NODE=node
say() { echo "setup-dev: $*"; }
[ -f "$LAPP/versions.json" ] || { say "no live checkout at $LAPP"; exit 1; }

if [ "${1:-}" = pair ]; then
  SECRET=$LIVE/sync.secret
  [ -s "$SECRET" ] || { (umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$SECRET"); say "made the bridge secret $SECRET"; }
  # each side: its checkout, the other hub's port, and its own name (it names the conflict copies, e.g. "x - dev copy.md")
  for side in "$LAPP:7778:live" "$DAPP:7777:dev"; do
    app=${side%%:*}; rest=${side#*:}; peer=${rest%%:*}; name=${rest#*:}
    "$NODE" -e '
      const fs = require("fs"), [f, peer, secret, name] = process.argv.slice(1);
      const c = fs.existsSync(f) ? JSON.parse(fs.readFileSync(f, "utf8")) : {};
      c.sync = { peer: "http://127.0.0.1:" + peer, secretFile: secret, name };
      fs.writeFileSync(f + ".tmp", JSON.stringify(c, null, 2) + "\n", { mode: 0o600 }); fs.renameSync(f + ".tmp", f);
    ' "$app/bridge/config.local.json" "$peer" "$SECRET" "$name"
    date -u +%Y-%m-%dT%H:%M:%SZ > "$app/bridge/.restart"
    say "bridge on in $app (peer 127.0.0.1:$peer); graceful restart asked"
  done
  exit 0
fi

mkdir -p "$DEV"
# 1. the checkout
if [ ! -d "$DAPP/.git" ]; then
  ORIGIN=$(git -C "$LAPP" remote get-url origin)
  git clone -q --branch develop "$ORIGIN" "$DAPP"
  say "cloned develop into $DAPP"
fi
git -C "$DAPP" fetch -q origin develop
[ "$(git -C "$DAPP" rev-parse --abbrev-ref HEAD)" = develop ] || { say "$DAPP is not on develop; fix it by hand"; exit 1; }
# 2. the data (once: after that the bridge keeps it in step)
if [ ! -f "$DEV/.seeded" ]; then
  LIST=''
  for p in bridge/auth.json bridge/state.json bridge/work.json bridge/work-current.json bridge/api-keys.json bridge/hub-state.json bridge/release-key.json bridge/hub-signing.key bridge/usage.json bridge/assistant.json bridge/providers.json bridge/forgejo.json bridge/forgejo-users.json bridge/server-owners.json bridge/ai-hosts.json bridge/unity.json bridge/gateway bridge/profiles bridge/profiles-deleted Chat Archive Board Notes Avatars Images Api Hyperwork; do
    [ -e "$LAPP/$p" ] && LIST="$LIST $p"
  done
  # shellcheck disable=SC2086
  (cd "$LAPP" && umask 007 && tar -cf - --exclude='*.tmp' --exclude='*.log' $LIST) | (cd "$DAPP" && umask 007 && tar -xf -)
  date -u +%Y-%m-%dT%H:%M:%SZ > "$DEV/.seeded"
  say "copied live's data into the dev checkout (sign-in sessions, device unlocks and AI-host sessions left out)"
fi
# 3. the settings (made from live's every time, so a change of live's settings reaches dev by running this again;
#    a bridge setting dev already has is kept)
"$NODE" -e '
  const fs = require("fs"), [src, dst, url] = process.argv.slice(1);
  const c = JSON.parse(fs.readFileSync(src, "utf8"));
  let old = {}; try { old = JSON.parse(fs.readFileSync(dst, "utf8")); } catch {}
  c.port = 7778; c.instance = "dev"; c.publicUrl = url;
  c.rustHub = Object.assign({}, c.rustHub || {}, { backPort: 7781 });
  c.aiNodes = {};
  c.allowedOrigins = [url];
  if (old.sync) c.sync = old.sync; else delete c.sync;
  if (old.devAllow) c.devAllow = old.devAllow;
  fs.writeFileSync(dst + ".tmp", JSON.stringify(c, null, 2) + "\n", { mode: 0o600 }); fs.renameSync(dst + ".tmp", dst);
' "$LAPP/bridge/config.local.json" "$DAPP/bridge/config.local.json" "$URL"
say "settings: port 7778, instance dev, $URL"
# 4. native builds to start with
if [ ! -x "$DAPP/native/dist/x86_64-unknown-linux-gnu/vr-run" ] && [ -d "$LAPP/native/dist" ]; then
  mkdir -p "$DAPP/native"; cp -a "$LAPP/native/dist" "$DAPP/native/"
  say "copied live's native builds to start with (the dev auto-deploy builds develop's own, clean)"
fi
# 5. the services
[ -n "$UNITS" ] || { say "no services (VR_DEV_UNITS empty): done"; exit 0; }
mkdir -p "$UNITS"
for u in viberoller-dev-hub.service viberoller-dev-autodeploy.service viberoller-dev-autodeploy.timer; do
  sed -e "s#__LIVE__#$LIVE#g" -e "s#__DEV__#$DEV#g" "$DAPP/deploy/$u" > "$UNITS/$u"
done
systemctl --user daemon-reload
[ "${1:-}" = --no-start ] && { say "installed; not started (--no-start)"; exit 0; }
# the dev hub may only run a build that keeps the one-hub jobs off (plans/dev-hub.md § 2b): else it would mirror every
# board change to Forgejo a second time, fire every scheduled workflow twice, and so on
grep -rqs '"instance"' "$DAPP/native/crates/vr-hub/src" || { say "develop's hub does not know \"instance\" yet (the one-hub jobs would run twice); not started"; exit 1; }
# ... and the build it runs must be develop's own, not the copy of live's from step 4 (an older vr-hub without the
# instance would run them all the same): build it now (clean, as the auto-deploy does), try it, put it in service
LT=x86_64-unknown-linux-gnu; WT=x86_64-pc-windows-gnu; HEADC=$(git -C "$DAPP" rev-parse HEAD)
BUILT=$(sed -n 's/.*"commit":"\([0-9a-f]*\)".*/\1/p' "$DAPP/native/dist/$LT/BUILD.json" 2>/dev/null | head -n 1)
if [ -z "$BUILT" ] || ! git -C "$DAPP" cat-file -e "$BUILT^{commit}" 2>/dev/null || ! git -C "$DAPP" diff --quiet "$BUILT" HEAD -- native/; then
  say "building develop's own native programs first (clean; a few minutes; $DEV/native-build.log)"
  (cd "$DAPP" && VR_COMMIT=$HEADC VR_STAGE_ONLY=1 nice -n 10 sh native/build.sh > "$DEV/native-build.log" 2>&1) || { say "the native build failed (see $DEV/native-build.log); not started"; exit 1; }
  sh "$DAPP/deploy/trial.sh" "$DAPP/native/dist/$LT/$HEADC/vr-hub" "$DAPP" >> "$DEV/native-build.log" 2>&1 || { say "develop's build failed its trial (see $DEV/native-build.log); not started"; exit 1; }
  for t in $LT $WT; do [ -d "$DAPP/native/dist/$t/$HEADC" ] && sh "$DAPP/native/mark-good.sh" "$DAPP/native/dist/$t" "$HEADC" >> "$DEV/native-build.log" 2>&1 && sh "$DAPP/native/use-build.sh" "$DAPP/native/dist/$t" "$HEADC" >> "$DEV/native-build.log" 2>&1; done
  say "develop's build $(echo "$HEADC" | cut -c1-7) is in service"
fi
systemctl --user enable --now viberoller-dev-hub.service viberoller-dev-autodeploy.timer
i=0; until curl -fsS -m 3 http://127.0.0.1:7778/api/status >/dev/null 2>&1; do i=$((i + 1)); [ "$i" -ge 60 ] && { say "the dev hub did not answer within a minute (journalctl --user -u viberoller-dev-hub)"; exit 1; }; sleep 1; done
mkdir -p "$DEV/deploy-state"
git -C "$DAPP" rev-parse HEAD > "$DEV/deploy-state/last-good"
say "the dev hub answers on 127.0.0.1:7778 ($(curl -fsS -m 3 http://127.0.0.1:7778/api/status))"
