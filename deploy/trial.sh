#!/usr/bin/env sh
# A build tried before it is swapped in (FR5, team note plans/full-rust-hub.md; R178):
#   sh deploy/trial.sh <vr-hub binary> <checkout> [port]
# Copies the hub's settings and data (never the originals: a private temporary root) and the page, starts the binary
# there on a free port, and expects within 30 s: /api/status 200 naming the checkout's version, / 200, /__vr-front
# naming the front (all answered by the build itself; the JavaScript hub is not there). Then stops it and removes the
# copy. Exit 0 = the build is good; 1 = it is not (the reason on stdout), and
# the auto-deploy keeps the old build. A build that cannot start fails at once.
set -u
BIN=$1; APP=$2; WANT=${3:-}
T=$(mktemp -d "${TMPDIR:-/tmp}/vr-trial-XXXXXX") || { echo "trial: no temporary folder"; exit 1; }
PID=''
cleanup() { if [ -n "$PID" ]; then kill "$PID" 2>/dev/null; sleep 0.3; kill -9 "$PID" 2>/dev/null; fi; rm -rf "$T"; }
trap cleanup EXIT INT TERM
fail() { echo "trial: $*"; exit 1; }
[ -x "$BIN" ] || fail "no such binary: $BIN"
[ -f "$APP/versions.json" ] || fail "no versions.json in $APP"
V=$(sed -n 's/.*"project"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$APP/versions.json" | head -n 1)
# the copy: settings, the small data, the page (the big folders stay out: pictures, archives, API runs)
LIST=''
for p in versions.json web bridge/config.json bridge/config.local.json bridge/profiles bridge/gateway Chat Board Notes; do [ -e "$APP/$p" ] && LIST="$LIST $p"; done
for p in "$APP"/bridge/*.json; do [ -f "$p" ] && LIST="$LIST bridge/$(basename "$p")"; done
# shellcheck disable=SC2086
(cd "$APP" && tar -cf - --exclude='*.tmp' --exclude='*.log' $LIST 2>/dev/null) | (cd "$T" && tar -xf -) || fail "could not copy the data"
chmod 700 "$T"
rm -f "$T/bridge/.restart" "$T/bridge/.rolled-back" "$T/bridge/.fell-back"
# a free port: the hub says 2 when its port is taken, so we try until it does not
tried=0
for p in ${WANT:-17901 17902 17903 17904 17905 17906 17907 17908 17909 17910}; do
  tried=$((tried + 1))
  "$BIN" --port "$p" --back "$((p + 1))" --root "$T/bridge" > "$T/trial.log" 2>&1 &
  PID=$!
  sleep 0.7
  if kill -0 "$PID" 2>/dev/null; then PORT=$p; break; fi
  wait "$PID"; code=$?; PID=''
  [ "$code" = 2 ] && continue
  fail "the build exited with code $code before answering (log: $(tail -n 3 "$T/trial.log" | tr '\n' ' '))"
done
[ -n "${PORT:-}" ] || fail "no free port among $tried tried"
i=0
while [ $i -lt 60 ]; do
  kill -0 "$PID" 2>/dev/null || { wait "$PID"; code=$?; PID=''; fail "the build exited with code $code while starting (log: $(tail -n 3 "$T/trial.log" | tr '\n' ' '))"; }
  st=$(curl -fsS -m 2 "http://127.0.0.1:$PORT/api/status" 2>/dev/null || true)
  if [ -n "$st" ]; then
    case "$st" in *'"hub":true'*) ;; *) fail "/api/status answered without hub:true: $st";; esac
    case "$st" in *"\"version\":\"$V\""*) ;; *) fail "/api/status names another version than $V: $st";; esac
    page=$(curl -s -o /dev/null -m 5 -w '%{http_code}' "http://127.0.0.1:$PORT/" 2>/dev/null)
    [ "$page" = 200 ] || fail "/ answered $page, not 200"
    # the front's own word (answered by this build itself, never passed on): its version, the modules it answers
    front=$(curl -fsS -m 5 "http://127.0.0.1:$PORT/__vr-front" 2>/dev/null || true)
    case "$front" in *'"front":"vr-hub"'*) ;; *) fail "/__vr-front answered without front:vr-hub: $front";; esac
    echo "trial: OK: $BIN answers on $PORT as VibeRoller $V (/api/status, /, /__vr-front) after $((i / 2)) s"
    exit 0
  fi
  sleep 0.5; i=$((i + 1))
done
fail "no answer on $PORT within 30 s (log: $(tail -n 3 "$T/trial.log" | tr '\n' ' '))"
