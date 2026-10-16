#!/usr/bin/env sh
# Starts the Mills hub: one process serves every account (mills, sponge, gee, ...).
cd "$(dirname "$0")"
command -v node >/dev/null || { echo "Node.js is required: https://nodejs.org"; exit 1; }
command -v claude >/dev/null || { echo "Claude Code is required: npm install -g @anthropic-ai/claude-code"; exit 1; }
if [ ! -f auth.json ]; then
  printf 'First run: enter the Mills password for the "mills" account: '; read -r MILLS_PW
  node set-password.js "$MILLS_PW" || exit 1
fi
# run.js = supervisor: owns the tunnel, runs server.js under it and restarts it when asked
# (node restart.js) or when it crashes.
exec node run.js
