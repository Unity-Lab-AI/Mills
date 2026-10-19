#!/usr/bin/env sh
# VibeRoller AI host tool for Linux (the black heart): switch this machine's AI hosting back on.
#   - with the VibeRollerAIHost service installed (install-ai-host-service.sh): start it
#   - else: clear the pause switch and start the node (ai-node.json present) or the hub here if nothing runs yet
cd "$(dirname "$0")/.." || exit 1
rm -f ai-host.off
if systemctl cat viberoller-ai-host >/dev/null 2>&1; then
  systemctl start viberoller-ai-host && echo "This machine is an AI host for VibeRoller again (viberoller-ai-host service)."; exit $?
fi
if [ -f ai-node.json ]; then
  if [ -f ai-node.lock ] && kill -0 "$(cat ai-node.lock)" 2>/dev/null; then echo "This machine's AI node is on again."; exit 0; fi
  nohup sh ./start-node.sh >/dev/null 2>&1 &
  echo "This machine's AI node is starting: it lends Unity's engines to the VibeRoller server. Tip: sudo sh host-tools/install-ai-host-service.sh makes it start at boot."; exit 0
fi
PORT=$(sed -n 's/.*"port"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' config.json | head -n 1); PORT=${PORT:-7777}
if curl -fsS -m 3 "http://127.0.0.1:$PORT/api/status" >/dev/null 2>&1; then echo "This machine hosts AI for VibeRoller again."; exit 0; fi
if systemctl --user cat viberoller >/dev/null 2>&1; then systemctl --user start viberoller; else nohup ./start.sh >/dev/null 2>&1 & fi
echo "VibeRoller is starting on this machine, with its AI switched on."
