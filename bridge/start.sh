#!/usr/bin/env sh
# Usage: ./start.sh            (Alfreddo's bridge)
#        ./start.sh sponge     (Sponge's bridge — needs profiles/sponge/config.json)
cd "$(dirname "$0")"
PROFILE="$1"
AUTH=auth.json; ARGS=""
if [ -n "$PROFILE" ]; then AUTH="profiles/$PROFILE/auth.json"; ARGS="--profile $PROFILE"; fi
command -v node >/dev/null || { echo "Node.js is required: https://nodejs.org"; exit 1; }
command -v claude >/dev/null || { echo "Claude Code is required: npm install -g @anthropic-ai/claude-code"; exit 1; }
if [ ! -f "$AUTH" ]; then
  printf 'First run: enter the Mills password for %s: ' "${PROFILE:-Alfreddo}"; read -r MILLS_PW
  # shellcheck disable=SC2086
  node set-password.js $ARGS "$MILLS_PW" || exit 1
fi
# shellcheck disable=SC2086
exec node server.js $ARGS
