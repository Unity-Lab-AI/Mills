#!/usr/bin/env sh
# Kept builds (FR3, team note plans/full-rust-hub.md; R176):
#   sh native/keep-builds.sh <dist/target> <current commit> [good list]
# <dist/target>/<commit>/ holds one build each (the binaries, BUILD.json, SHA256SUMS; native/build.sh stages them there,
# native/use-build.sh puts one in service). Keeps the current build and the 2 newest good ones (the good list: one
# commit per line, newest first, written by the auto-deploy when a version is verified; default <dist/target>/good),
# deletes every other build folder, and writes <dist/target>/current (the current commit, a plain file: Windows too).
# The current build is never deleted, whatever the list says.
set -eu
DIST=$1; CUR=$2; GOOD=${3:-$1/good}
[ -d "$DIST/$CUR" ] || { echo "keep-builds: no build $DIST/$CUR" >&2; exit 1; }
printf '%s\n' "$CUR" > "$DIST/current.tmp" && mv "$DIST/current.tmp" "$DIST/current"
KEEP=" $CUR "
n=0
if [ -f "$GOOD" ]; then
  while read -r c; do
    [ -n "$c" ] || continue
    [ "$c" = "$CUR" ] && continue
    [ -d "$DIST/$c" ] || continue
    KEEP="$KEEP$c "; n=$((n + 1))
    [ "$n" -ge 2 ] && break
  done < "$GOOD"
fi
for d in "$DIST"/*/; do
  [ -d "$d" ] || continue
  c=$(basename "${d%/}")
  case "$KEEP" in *" $c "*) continue;; esac
  case "$c" in *[!0-9a-z-]*) continue;; esac # only build folders (a commit, or a stand-in name)
  rm -rf "${d%/}"
done
echo "keep-builds: kept$KEEP(current $CUR)"
