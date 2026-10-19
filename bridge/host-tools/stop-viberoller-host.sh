#!/usr/bin/env sh
# VibeRoller AI host tool for Linux (the knife): stop this machine's AI hosting. VibeRoller on the server keeps running
# for everyone (chat, board, calls, notes, people's own AI keys); only the AI hosted here stops.
#   - with the VibeRollerAIHost service installed (install-ai-host-service.sh): stop it (it starts again at the next boot)
#   - else: write the pause switch ai-host.off; the node / hub here pauses its AI until the heart clears it
cd "$(dirname "$0")/.." || exit 1
if systemctl cat viberoller-ai-host >/dev/null 2>&1; then
  systemctl stop viberoller-ai-host && echo "This machine's AI host is stopped (viberoller-ai-host service). Start it again with start-viberoller-host.sh; it also starts at the next boot."; exit $?
fi
date "+AI paused by this machine's host at %Y-%m-%d %H:%M:%S" > ai-host.off
echo "This machine's AI is paused. Start it again with start-viberoller-host.sh."
