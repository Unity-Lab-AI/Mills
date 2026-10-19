#!/usr/bin/env sh
# VibeRoller: put the hub's data (from deploy/pack-data.cmd or .sh on the old host) into the server's live checkout,
# exactly as it is. Run as root, once, after setup-server.sh:   sudo sh deploy/restore-data.sh <viberoller-data-*.tar.gz>
# It stops the hub, unpacks, hands every file to the service user, counts the accounts, starts the hub again and
# checks that it answers. Nothing is reset: passwords, device locks and keys arrive as they were (hub laws 1, 2, 8).
set -eu
ARCHIVE=${1:-}; BASE=${BASE:-/srv/viberoller}; SVC=${SVC:-viberoller}; APP=$BASE/app
[ "$(id -u)" = 0 ] || { echo "Run as root (sudo)."; exit 1; }
[ -f "$ARCHIVE" ] || { echo "Usage: sudo sh deploy/restore-data.sh <viberoller-data-*.tar.gz>"; exit 1; }
[ -d "$APP/bridge" ] || { echo "$APP is not the live checkout: run deploy/setup-server.sh first."; exit 1; }
tar -tzf "$ARCHIVE" | grep -qv '^\(bridge/\(auth\|state\|hub-state\|usage\|assistant\|providers\|forgejo\|forgejo-users\|server-owners\)\.json\|bridge/profiles\|bridge/profiles-deleted\|Chat\|Archive\|Board\|Notes\|Avatars\|Images\|Api\)' && { echo "The archive holds more than hub data (code or unknown folders): refusing. Pack it with deploy/pack-data."; exit 1; }
if [ -f "$APP/bridge/auth.json" ] || [ -d "$APP/bridge/profiles" ]; then
  echo "This server already has account data. Refusing to overwrite it. Move it away first if that is what you want:"; echo "  ls -la $APP/bridge/profiles $APP/bridge/auth.json"; exit 1
fi
systemctl stop viberoller-hub || true
tar -xzf "$ARCHIVE" -C "$APP"
chown -R "$SVC:$SVC" "$APP"
find "$APP/bridge" -maxdepth 1 -name '*.json' -exec chmod 600 {} \; 2>/dev/null || true
N=0; [ -f "$APP/bridge/auth.json" ] && N=1
for d in "$APP"/bridge/profiles/*/; do [ -f "$d/auth.json" ] && N=$((N + 1)); done
echo "accounts with a password file: $N"
[ -f "$APP/Chat/agent-key.json" ] && echo "Unity's chat key: present" || echo "Unity's chat key: MISSING (Chat/agent-key.json): Unity could not read the chat; check the archive"
systemctl start viberoller-hub; sleep 3
PORT=$(sed -n 's/.*"port"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' "$APP/bridge/config.local.json" | head -n 1); PORT=${PORT:-7777}
curl -fsS -m 5 "http://127.0.0.1:$PORT/api/status" >/dev/null && echo "the hub is up with the data" || { echo "the hub did not answer: journalctl -u viberoller-hub -n 50"; exit 1; }
echo "Now sign in at the public address with every account (a right password from a new device answers 'locked to another device': that proves it without touching the lock)."
