#!/usr/bin/env sh
# Put a kept build in service (FR3, team note plans/full-rust-hub.md):
#   sh native/use-build.sh <dist/target> <commit>
# Copies <dist/target>/<commit>/* (the binaries, BUILD.json, SHA256SUMS) over the files at <dist/target>/ (what run.js,
# vr-run, the Downloads and the AI hosts' updates read: that layout never changed), file by file through a rename, then
# keeps the current build + the 2 newest good ones (native/keep-builds.sh). Used by native/build.sh after a build (unless
# VR_STAGE_ONLY=1: the auto-deploy tries the build first, deploy/trial.sh) and by the auto-deploy's rollback, which
# restores the last good build with the checkout.
set -eu
DIST=$1; C=$2
[ -d "$DIST/$C" ] || { echo "use-build: no build $DIST/$C" >&2; exit 1; }
[ -f "$DIST/$C/BUILD.json" ] || { echo "use-build: $DIST/$C has no BUILD.json" >&2; exit 1; }
rm -rf "$DIST/.next"; mkdir -p "$DIST/.next"
for f in "$DIST/$C"/*; do [ -f "$f" ] && cp "$f" "$DIST/.next/"; done
for f in "$DIST/.next"/*; do [ -f "$f" ] && mv -f "$f" "$DIST/$(basename "$f")"; done
rmdir "$DIST/.next"
sh "$(dirname "$0")/keep-builds.sh" "$DIST" "$C"
echo "use-build: $DIST now serves $C"
