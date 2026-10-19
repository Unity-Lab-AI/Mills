#!/usr/bin/env sh
# VibeRoller: install the VibeRollerAIHost systemd service on Linux, once, so this machine lends Unity's engines to the
# VibeRoller server from boot (before anyone signs in). Run with sudo from the host-tools folder:
#   sudo sh install-ai-host-service.sh            install (or update) + start; the invoking user may start / stop it with the heart / knife
#   sudo sh install-ai-host-service.sh remove     stop and remove it
# Needs Node.js on the PATH and ../ai-node.json (from ../ai-node.example.json: the hub address, this machine's node name, its token).
set -eu
HERE=$(cd "$(dirname "$0")" && pwd); DIR=$(cd "$HERE/.." && pwd)
NAME=viberoller-ai-host; UNIT=/etc/systemd/system/$NAME.service
[ "$(id -u)" = 0 ] || { echo "Run with sudo."; exit 1; }
WHO=${SUDO_USER:-$(id -un)}
if [ "${1:-}" = remove ]; then
  systemctl disable --now $NAME 2>/dev/null || true; rm -f "$UNIT" /etc/polkit-1/rules.d/50-viberoller-ai-host.rules; systemctl daemon-reload
  echo "The $NAME service is removed."; exit 0
fi
[ -f "$DIR/ai-node.js" ] || { echo "ai-node.js was not found in $DIR."; exit 1; }
command -v node >/dev/null || { echo "Node.js was not found on the PATH: install it and run this again."; exit 1; }
sed -e "s#__USER__#$WHO#" -e "s#__DIR__#$DIR#" "$HERE/viberoller-ai-host.service" > "$UNIT"
# the installing user may start / stop the service without a password (the heart and the knife do that)
if [ -d /etc/polkit-1/rules.d ]; then
  cat > /etc/polkit-1/rules.d/50-viberoller-ai-host.rules <<EOF
polkit.addRule(function(action, subject) {
  if (action.id == "org.freedesktop.systemd1.manage-units" && action.lookup("unit") == "$NAME.service" && subject.user == "$WHO") return polkit.Result.YES;
});
EOF
fi
systemctl daemon-reload; systemctl enable $NAME >/dev/null
[ -f "$DIR/ai-node.json" ] || echo "NOTE: $DIR/ai-node.json is missing. Copy ai-node.example.json to ai-node.json and fill in the hub address, this machine's node name and its token; the service keeps retrying until it is there."
systemctl restart $NAME; sleep 2
systemctl is-active --quiet $NAME && echo "The $NAME service is installed, running, and starts at every boot (journalctl -u $NAME -f)." || echo "Installed, but not running yet: journalctl -u $NAME -n 30"
echo "The heart (start-viberoller-host.sh) starts it, the knife (stop-viberoller-host.sh) stops it."
