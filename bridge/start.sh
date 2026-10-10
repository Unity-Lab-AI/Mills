#!/usr/bin/env sh
cd "$(dirname "$0")"
command -v node >/dev/null || { echo "Node.js is required: https://nodejs.org"; exit 1; }
command -v claude >/dev/null || { echo "Claude Code is required: npm install -g @anthropic-ai/claude-code"; exit 1; }
if [ ! -f auth.json ]; then
  printf 'First run: enter the Mills password you were given: '; read -r MILLS_PW
  node set-password.js "$MILLS_PW" || exit 1
fi
exec node server.js
