#!/usr/bin/env sh
# VibeRoller: pack this hub's DATA for the move to the server, exactly as it is (accounts with their password hashes
# and device locks, chat, keys, board, notes, pictures, saved sessions, settings). Nothing is reset or changed, and no
# code is included. Run on the host from the VibeRoller folder, WITH THE HUB STOPPED:
#   sh deploy/pack-data.sh          → ~/viberoller-data-<date>.tar.gz
#   sh deploy/pack-data.sh ovh      → the same, then copied with scp to <alias>:/srv/apps/VibeRoller/ for restore-data.sh
set -eu
cd "$(dirname "$0")/.."
PORT=$(sed -n 's/.*"port"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' bridge/config.local.json bridge/config.json 2>/dev/null | head -n 1); PORT=${PORT:-7777}
curl -fsS -m 2 "http://127.0.0.1:$PORT/api/status" >/dev/null 2>&1 && { echo "The hub is still running here. Stop it first, then run this again."; exit 1; }
OUT=$HOME/viberoller-data-$(date +%Y%m%d-%H%M).tar.gz
LIST=''
for p in bridge/auth.json bridge/state.json bridge/work.json bridge/work-current.json bridge/api-keys.json bridge/hub-state.json bridge/front-sessions.json bridge/device-locks.json bridge/release-key.json bridge/usage.json bridge/assistant.json bridge/providers.json bridge/forgejo.json bridge/forgejo-users.json bridge/server-owners.json bridge/ai-hosts.json bridge/profiles bridge/profiles-deleted Chat Archive Board Notes Avatars Images Api; do
  [ -e "$p" ] && LIST="$LIST $p"
done
[ -n "$LIST" ] || { echo "Nothing to pack here: no data folders found next to bridge/."; exit 1; }
# shellcheck disable=SC2086
tar -czf "$OUT" --exclude='*.tmp' --exclude='*.log' $LIST
echo "Packed:$LIST"
echo "→ $OUT"
if [ -n "${1:-}" ]; then
  scp "$OUT" "$1:/srv/apps/VibeRoller/" && echo "Copied. On the server: sh /srv/apps/VibeRoller/app/deploy/restore-data.sh /srv/apps/VibeRoller/$(basename "$OUT")"
else
  echo "Copy that file to the server (e.g. scp \"$OUT\" ovh:/srv/apps/VibeRoller/), then there: sh deploy/restore-data.sh $(basename "$OUT")"
fi
echo "(bridge/config.json, bridge/tunnel.json and the code are not included on purpose.)"
