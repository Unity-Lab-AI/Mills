#!/bin/sh
# Headroom (headroomlabs-ai/headroom, Apache 2.0) for the dev hub (5.36.0): a local context-compression proxy the dev
# hub's relayed Claude Code runs go through (config headroom.url; plans/dev-hub.md). No root: uv and Headroom live under
# $LIVE/tools, the proxy is a user service of the hub's account bound to 127.0.0.1, its upstream is the dev hub's
# /llm-relay. No telemetry, no subscription polling, no model downloads (HF_HUB_OFFLINE).
#
#   sh deploy/setup-headroom.sh            install (or repair) Headroom, start its service, point the dev hub at it
#   sh deploy/setup-headroom.sh off        stop it and take headroom out of the dev hub's settings (runs go straight
#                                          to the relay again; nothing is uninstalled)
# Environment: VR_LIVE_BASE (default /srv/apps/VibeRoller), VR_DEV_BASE (default $VR_LIVE_BASE/dev), VR_HEADROOM_PORT
#   (default 7787), VR_HEADROOM_VERSION (default 0.42.0), VR_DEV_UNITS (default the account home .config/systemd/user).
# The dev hub reads headroom.url at start: this asks it for a graceful restart.
set -eu
LIVE=${VR_LIVE_BASE:-/srv/apps/VibeRoller}
DEV=${VR_DEV_BASE:-$LIVE/dev}
PORT=${VR_HEADROOM_PORT:-7787}
VER=${VR_HEADROOM_VERSION:-0.42.0}
UHOME=$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f6); UHOME=${UHOME:-$HOME}
UNITS=${VR_DEV_UNITS-$UHOME/.config/systemd/user}
HR=$LIVE/tools/headroom; DCFG=$DEV/app/bridge/config.local.json
NODE=$LIVE/node/current/bin/node; [ -x "$NODE" ] || NODE=node
here=$(cd "$(dirname "$0")" && pwd)
say() { echo "setup-headroom: $*"; }
[ -f "$DCFG" ] || { say "no dev hub settings at $DCFG (deploy/setup-dev.sh first)"; exit 1; }
# the dev hub's headroom.url: set (url) or taken out (""), the file stays owner-only
setcfg() {
  "$NODE" -e '
    const fs = require("fs"), [f, url] = process.argv.slice(1);
    const c = JSON.parse(fs.readFileSync(f, "utf8"));
    if (url) c.headroom = { url }; else delete c.headroom;
    fs.writeFileSync(f + ".tmp", JSON.stringify(c, null, 2) + "\n", { mode: 0o600 }); fs.renameSync(f + ".tmp", f);
  ' "$DCFG" "$1"
  date -u +%FT%TZ > "$DEV/app/bridge/.restart"
}
if [ "${1:-}" = off ]; then
  [ -z "$UNITS" ] || systemctl --user disable --now viberoller-dev-headroom.service 2>/dev/null || true
  setcfg ""; say "off: the dev hub's runs go straight to the relay (graceful restart asked)"; exit 0
fi
# 1. uv (a single binary, checksummed) and Headroom with its proxy, in their own folders
mkdir -p "$HR/bin" "$HR/home" "$HR/dl"
if [ ! -x "$HR/bin/uv" ]; then
  T=$(mktemp -d "$HR/dl/uv-XXXXXX")
  A=https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-unknown-linux-gnu.tar.gz
  curl -fsSL -m 120 -o "$T/uv.tgz" "$A" && curl -fsSL -m 30 -o "$T/uv.sha" "$A.sha256"
  (cd "$T" && echo "$(cut -d' ' -f1 uv.sha)  uv.tgz" | sha256sum -c -) || { say "uv download did not match its checksum"; rm -rf "$T"; exit 1; }
  tar -xzf "$T/uv.tgz" -C "$HR/bin" --strip-components=1; rm -rf "$T"
fi
export UV_TOOL_DIR=$HR/tools UV_TOOL_BIN_DIR=$HR/bin UV_CACHE_DIR=$HR/cache UV_PYTHON_INSTALL_DIR=$HR/python HEADROOM_TELEMETRY=off DO_NOT_TRACK=1
"$HR/bin/uv" tool install --force --python 3.13 "headroom-ai[proxy]==$VER" >/dev/null
"$HR/bin/uv" cache clean >/dev/null 2>&1 || true
say "Headroom $("$HR/bin/headroom" --version 2>/dev/null | tail -1) in $HR"
# 2. its user service, the dev hub's relay as upstream
HUB=$("$NODE" -e 'const c = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")); console.log(c.port || 7778)' "$DCFG")
[ -n "$UNITS" ] || { say "no services (VR_DEV_UNITS empty): done"; exit 0; }
mkdir -p "$UNITS"
sed -e "s|__LIVE__|$LIVE|g" -e "s|__PORT__|$PORT|g" -e "s|__HUB__|$HUB|g" "$here/viberoller-dev-headroom.service" > "$UNITS/viberoller-dev-headroom.service"
systemctl --user daemon-reload
systemctl --user enable --now viberoller-dev-headroom.service
systemctl --user restart viberoller-dev-headroom.service
ok=""; for i in $(seq 1 60); do if curl -fsS -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then ok=1; break; fi; sleep 1; done
[ -n "$ok" ] || { say "Headroom did not answer on 127.0.0.1:$PORT/health; the dev hub is left as it was (journalctl --user -u viberoller-dev-headroom)"; exit 1; }
# 3. the dev hub sends its relayed runs through it
setcfg "http://127.0.0.1:$PORT"
say "on: the dev hub's relayed Claude Code runs go through Headroom on 127.0.0.1:$PORT (graceful restart asked)"
