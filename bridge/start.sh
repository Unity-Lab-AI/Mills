#!/usr/bin/env sh
# Starts the VibeRoller hub: one process serves every account. To start it by itself at boot, see viberoller.service.
cd "$(dirname "$0")"
command -v node >/dev/null || { echo "Node.js is required: https://nodejs.org"; exit 1; }
command -v claude >/dev/null || { echo "Claude Code is required: npm install -g @anthropic-ai/claude-code"; exit 1; }
if [ ! -f auth.json ]; then
  printf 'First run: enter the VibeRoller password for the "mills" account: '; read -r MILLS_PW
  node set-password.js "$MILLS_PW" || exit 1
fi
# vr-run (the Rust supervisor, native/crates/vr-run; FR4 in the team note plans/full-rust-hub.md) when it is built here:
# it runs the hub from the kept builds (native/dist/<target>/<commit>), health-checks it and falls back to the previous
# good build when a new one is broken. Else run.js, the JavaScript supervisor: owns the tunnel, runs server.js and
# restarts it when asked (node restart.js) or when it crashes.
T=x86_64-unknown-linux-gnu
if [ -x "../native/dist/$T/vr-run" ] && [ -f "../native/dist/$T/current" ]; then
  P=$(node -e "console.log(require('./common').loadProfile('').config.port)" 2>/dev/null || echo 7777)
  # config "rustHub": { "alone": true } = no JavaScript hub at all (vr-run reads it too; said here for the log)
  exec env VR_NODE="$(command -v node)" "../native/dist/$T/vr-run" --root "$(pwd)" --port "$P" --builds "$(cd "../native/dist/$T" && pwd)"
fi
exec node run.js
