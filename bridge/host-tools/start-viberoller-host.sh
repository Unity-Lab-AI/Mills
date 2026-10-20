#!/usr/bin/env sh
# VibeRoller AI host on Linux: start (the old heart). The control app does it all now: sh host-tools/viberoller-host-control.sh
# (status, start / stop / restart / pause, update, every setting, start at boot). This stays for scripts and old shortcuts.
cd "$(dirname "$0")/.." || exit 1
exec node vr-host.js start
