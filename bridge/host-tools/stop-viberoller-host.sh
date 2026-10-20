#!/usr/bin/env sh
# VibeRoller AI host on Linux: stop (the old knife). The control app does it all now: sh host-tools/viberoller-host-control.sh
# (status, start / stop / restart / pause, update, every setting, start at boot). This stays for scripts and old shortcuts.
# It stops as soon as nothing is in flight; --now stops at once.
cd "$(dirname "$0")/.." || exit 1
exec node vr-host.js stop "$@"
