#!/usr/bin/env sh
# Starts this machine's VibeRoller AI node (bridge/ai-node.js): it lends Unity's local engines and its AI to the hub
# named in bridge/ai-node.json, connecting out. A second copy finds the first and exits 0. To run it at boot, use
# viberoller.service with ExecStart=/usr/bin/env node ai-node.js instead of run.js (same steps as in that file).
# Exit codes: 0 = stopped, 2 = already running (both end this script); 3 = new code or an update arrived: start again at
# once; anything else = it crashed: start again after 10 seconds. Ctrl+C stops it.
cd "$(dirname "$0")" || exit 1
command -v node >/dev/null || { echo "Node.js is required: https://nodejs.org"; exit 1; }
[ -f ai-node.json ] || { echo "bridge/ai-node.json is missing: copy ai-node.example.json to ai-node.json and fill in the hub, name and token."; exit 1; }
while :; do
  node ai-node.js; rc=$?
  case $rc in
    0|2) exit 0 ;;
    3) ;;
    *) echo "The AI node stopped with an error ($rc); starting it again in 10 seconds (Ctrl+C stops it)."; sleep 10 ;;
  esac
done
