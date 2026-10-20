#!/usr/bin/env sh
# Vibe Roller Desktop (Linux): VibeRoller Chat in its own window, and its own installer + updater.
#   (no option)  installed copy → open VibeRoller, then check the hub for a newer package and update itself.
#                downloaded package → an earlier install is found → update it and open; none → ask: install or portable.
#   --install    install (or update) into ~/.local/share/viberoller/desktop, with an application-menu entry
#   --portable   open VibeRoller from this folder, installing nothing
#   --update     update the install that was found to this package (nothing else)
#   --status     what is installed where, and its version
#   --yes        no questions (install when nothing is found)
# Earlier installs: the standard folder, and the launcher older packages copied to ~/.local/bin/viberoller-desktop.
# Files are replaced by writing a new file and renaming it over the old one, so a copy of this script that is running
# right now (sh reads a script as it goes) keeps reading its own, unchanged file.
HUB='__HUB__'
URL="$HUB/index.html?app=1"
HERE=$(cd "$(dirname "$(readlink -f "$0" 2>/dev/null || echo "$0")")" && pwd) # through the ~/.local/bin link too
STD="$HOME/.local/share/viberoller/desktop"
PKG=desktop-linux
MODE=''; YES=''
for a in "$@"; do case $a in --install|--portable|--update|--status) MODE=$a ;; --yes) YES=1 ;; esac; done

ver() { if [ -f "$1/VERSION.txt" ]; then cat "$1/VERSION.txt"; elif [ -f "$1/viberoller-desktop.sh" ] || [ "$1" = legacy ]; then echo "(older, no VERSION.txt)"; fi; }
launch() {
  for b in google-chrome google-chrome-stable chromium chromium-browser brave-browser brave microsoft-edge microsoft-edge-stable; do
    if command -v "$b" >/dev/null 2>&1; then nohup "$b" --app="$URL" --window-size=1280,860 >/dev/null 2>&1 & return 0; fi
  done
  nohup xdg-open "$URL" >/dev/null 2>&1 &
}
put() { mkdir -p "$(dirname "$2")"; cp "$1" "$2.vrnew" && mv -f "$2.vrnew" "$2"; } # new file + rename, never in place
install_into() {
  mkdir -p "$1"
  for f in "$HERE"/*; do [ -f "$f" ] && put "$f" "$1/$(basename "$f")"; done
  chmod +x "$1/viberoller-desktop.sh"
  mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/scalable/apps"
  ln -sf "$1/viberoller-desktop.sh" "$HOME/.local/bin/viberoller-desktop.vrnew" && mv -f "$HOME/.local/bin/viberoller-desktop.vrnew" "$HOME/.local/bin/viberoller-desktop"
  curl -fsSL -m 10 "$HUB/icons/mills.svg" -o "$HOME/.local/share/icons/hicolor/scalable/apps/viberoller.svg" 2>/dev/null || true
  cat > "$HOME/.local/share/applications/viberoller-desktop.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=VibeRoller Desktop
Comment=VibeRoller Chat in its own window
Exec=$1/viberoller-desktop.sh
Icon=viberoller
Terminal=false
Categories=Network;Chat;
EOF
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
}
unpack() { # zip → folder, with whatever this machine has
  if command -v unzip >/dev/null 2>&1; then unzip -qo "$1" -d "$2"
  elif command -v python3 >/dev/null 2>&1; then python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$1" "$2"
  elif command -v bsdtar >/dev/null 2>&1; then bsdtar -xf "$1" -C "$2"
  else return 1; fi
}
self_update() { # an installed copy keeps itself current: the hub names the package version it would hand out now
  want=$(curl -fsS -m 8 "$HUB/api/pub/versions" 2>/dev/null | sed -n "s/.*\"$PKG\":\"\([^\"]*\)\".*/\1/p")
  [ -n "$want" ] && [ "$want" != "$(ver "$STD")" ] || return 0
  tmp=$(mktemp -d) || return 0
  if curl -fsSL -m 60 "$HUB/api/pub/$PKG.zip" -o "$tmp/p.zip" && unpack "$tmp/p.zip" "$tmp"; then sh "$tmp/VibeRollerDesktop/viberoller-desktop.sh" --update --yes >/dev/null 2>&1; fi
  rm -rf "$tmp"
}
find_install() { # the standard folder, or the launcher an older package copied into ~/.local/bin
  if [ -d "$STD" ] && [ "$STD" != "$HERE" ]; then echo "$STD"; return; fi
  [ -f "$HOME/.local/bin/viberoller-desktop" ] && [ ! -L "$HOME/.local/bin/viberoller-desktop" ] && echo legacy
}

MINE=$(ver "$HERE")
if [ "$MODE" = --status ]; then
  echo "This package: $HERE ($MINE)"
  old=$(find_install); if [ -n "$old" ]; then echo "Install: $old ($(ver "$old"))"; else echo "No other install found."; fi
  exit 0
fi
[ "$MODE" = --portable ] && { launch; exit 0; }
if [ "$HERE" = "$STD" ] && [ -z "$MODE" ]; then launch; self_update; exit 0; fi
old=$(find_install)
if [ -n "$old" ] && [ "$MODE" != --install ]; then
  if [ "$(ver "$old")" = "$MINE" ] && [ "$MODE" != --update ]; then launch; exit 0; fi
  install_into "$STD"
  [ "$MODE" = --update ] && exit 0
  echo "VibeRoller Desktop updated to $MINE."; launch; exit 0
fi
[ "$MODE" = --update ] && { echo "No earlier install found to update. Run without options (or with --install) to install."; exit 1; }
choice=i
if [ "$MODE" != --install ] && [ -z "$YES" ] && [ -t 0 ]; then printf 'Install VibeRoller Desktop into %s with an application-menu entry (I), or run it portable from this folder (P)? [I/p] ' "$STD"; read -r choice; fi
case $choice in p*|P*) launch; exit 0 ;; esac
install_into "$STD"
echo "VibeRoller Desktop is installed ($MINE): find it in your application menu. It keeps itself up to date."
launch
