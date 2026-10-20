#!/usr/bin/env sh
# Link YOUR Linux account on the server to your VibeRoller account, once. Run it as yourself (over SSH, or let your AI
# agent there run it); it needs no root and touches nothing but your own home:
#   sh /srv/apps/VibeRoller/app/deploy/link-linux-account.sh            link (safe to run again)
#   sh /srv/apps/VibeRoller/app/deploy/link-linux-account.sh unlink     take the hub's access away again (your files stay)
# It makes ~/uploaded_files (where the files you upload in VibeRoller land, yours to work on, edit and run) and lets the
# hub's Linux account read and write THAT folder only: an ACL on it (and on what is created in it), plus passing through
# your home folder to reach it (no listing, no reading anything else there).
set -eu
DIR="$HOME/uploaded_files"
APP=$(cd "$(dirname "$0")/.." && pwd)
HUB=$(stat -c %U "$APP" 2>/dev/null || ls -ld "$APP" | awk '{print $3}')
ME=$(id -un)
command -v setfacl >/dev/null 2>&1 || { echo "setfacl is missing on this server (package: acl). Ask whoever has root to install it, then run this again."; exit 1; }
if [ "${1:-}" = unlink ]; then
  setfacl -x "u:$HUB" "$HOME" 2>/dev/null || true
  [ -d "$DIR" ] && setfacl -R -x "u:$HUB" "$DIR" 2>/dev/null && setfacl -R -d -x "u:$HUB" "$DIR" 2>/dev/null || true
  echo "Unlinked: the hub ($HUB) can no longer reach $DIR. Your files stay where they are."; exit 0
fi
mkdir -p "$DIR"
chmod 700 "$DIR"
if [ "$HUB" != "$ME" ]; then
  setfacl -m "u:$HUB:x" "$HOME"                          # pass through your home to reach the folder (not list or read it)
  setfacl -R -m "u:$HUB:rwX,u:$ME:rwX" "$DIR"            # the folder and what is in it already
  setfacl -R -d -m "u:$HUB:rwX,u:$ME:rwX,m::rwx" "$DIR"   # everything created in it later, by either of you
fi
echo "Linked: VibeRoller puts your uploads in $DIR (the hub runs as $HUB; it reaches this folder only)."
echo "Check it in VibeRoller: Files (on the menu) says \"Linked to your account $ME\"."
