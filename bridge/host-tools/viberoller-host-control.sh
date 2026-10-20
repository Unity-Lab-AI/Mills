#!/usr/bin/env sh
# VibeRoller AI Host · heart & knife: the control app (status, start / stop / restart / pause, update, every setting, start
# at boot). With arguments it runs one command: status | start | stop [--now] | restart [--now] | pause | resume | update |
# log | boot service|user|off | settings | set <setting> <value>.
cd "$(dirname "$0")/.." || exit 1
command -v node >/dev/null 2>&1 || { echo "Node.js is required: https://nodejs.org"; exit 1; }
exec node vr-host.js "$@"
