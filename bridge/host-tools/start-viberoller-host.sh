#!/usr/bin/env sh
# VibeRoller host tool for Linux (the black heart): switch this machine's AI hosting back on, and start
# VibeRoller here if it is not running (the systemd user service if installed, else start.sh in the background).
cd "$(dirname "$0")/.." || exit 1
rm -f ai-host.off
PORT=$(sed -n 's/.*"port"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' config.json | head -n 1); PORT=${PORT:-7777}
if curl -fsS -m 3 "http://127.0.0.1:$PORT/api/status" >/dev/null 2>&1; then echo "This machine hosts AI for VibeRoller again."; exit 0; fi
if systemctl --user cat viberoller >/dev/null 2>&1; then systemctl --user start viberoller; else nohup ./start.sh >/dev/null 2>&1 & fi
echo "VibeRoller is starting on this machine, with its AI switched on."
