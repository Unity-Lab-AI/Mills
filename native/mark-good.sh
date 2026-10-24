#!/usr/bin/env sh
# Mark a build good (T-157, the fallback chain):
#   sh native/mark-good.sh <dist/target> <commit>
# Puts <commit> first in <dist/target>/good (newest first, no repeats), the list native/keep-builds.sh keeps 2 builds
# from besides the current one. The auto-deploy calls it once a build passed its trial (deploy/trial.sh), before
# native/use-build.sh puts it in service: the hub version was verified before the build began, so a tried build is a good
# one. (Marking only at the hub's verify missed every build: the native build of a version runs after its verify, so the
# list stayed empty and keep-builds deleted every previous build, 5.13.3.)
set -eu
DIST=$1; C=$2
[ -d "$DIST/$C" ] || { echo "mark-good: no build $DIST/$C" >&2; exit 1; }
G=$DIST/good
{ echo "$C"; [ -f "$G" ] && grep -vx "$C" "$G" || true; } > "$G.tmp"
mv -f "$G.tmp" "$G"
echo "mark-good: $C is a good build in $DIST"
