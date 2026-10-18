#!/usr/bin/env sh
# VibeRoller host tool for Linux (the knife): pause this machine's AI hosting. VibeRoller keeps running for
# everyone (chat, board, calls, notes, people's own AI keys); only the AI hosted here waits.
cd "$(dirname "$0")/.." || exit 1
date "+AI paused by this machine's host at %Y-%m-%d %H:%M:%S" > ai-host.off
echo "This machine's AI is paused. Start it again with start-viberoller-host.sh."
