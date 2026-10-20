#!/bin/sh
# Starts the Linux desktop app headless and checks that it comes up: the Tools window loads its page, runs its script
# and reaches the app (vr-desktop --smoke exits 0 when the page reported). No root, nothing installed: Xvfb, its
# keymap compiler and WebKitGTK's runtime come from the user-space sysroot (native/tauri-env.sh), and since Xvfb and
# WebKit start helpers from fixed system paths (/usr/bin/xkbcomp, /usr/lib/x86_64-linux-gnu/webkit2gtk-4.1/…), the
# check runs in a user + mount namespace of its own where those folders show the system's files plus the sysroot's.
# Used after a Linux build and by the regression suite (R165).    sh native/smoke-desktop.sh [path/to/vr-desktop] [hub]
# With a hub address the main window loads that hub's page, which must also see the app and be allowed what the hub's
# page may do (app_info, notify), and show the Tools tile.
set -u
HUB=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
OWNER_HOME=${VR_TOOLCHAIN_HOME:-$(getent passwd "$(id -un)" | cut -d: -f6)}
S=${VR_TAURI_SYSROOT:-$OWNER_HOME/.local/opt/tauri-sysroot}
BIN=$(cd "$(dirname "${1:-$HERE/dist/x86_64-unknown-linux-gnu/vr-desktop}")" && pwd)/$(basename "${1:-vr-desktop}")
[ -x "$S/usr/bin/Xvfb" ] && [ -x "$S/usr/bin/xkbcomp" ] || { echo "SKIP: no Xvfb / xkbcomp in the sysroot ($S)"; exit 77; }
[ -x "$BIN" ] || { echo "SKIP: no desktop binary at $BIN"; exit 77; }
unshare -rm true 2>/dev/null || { echo "SKIP: no user namespaces here"; exit 77; }
TMP=$(mktemp -d)
# a folder that shows every entry of a system folder (through a bind of the original) plus the sysroot's extra ones
merge() { # $1 system folder, $2 sysroot folder, $3 name
  mkdir -p "$TMP/$3" "$TMP/orig-$3"
  for f in "$1"/* "$1"/.[!.]*; do [ -e "$f" ] || [ -L "$f" ] || continue; ln -s "$TMP/orig-$3/$(basename "$f")" "$TMP/$3/$(basename "$f")"; done
  for f in "$2"/*; do [ -e "$TMP/$3/$(basename "$f")" ] || [ -L "$TMP/$3/$(basename "$f")" ] || ln -s "$f" "$TMP/$3/$(basename "$f")"; done
}
merge /usr/bin "$S/usr/bin" bin
merge /usr/lib/x86_64-linux-gnu "$S/usr/lib/x86_64-linux-gnu" lib
merge /usr/share "$S/usr/share" share
D=$(( 90 + $(id -u) % 9 ))
mkdir -p "$TMP/home"
cat > "$TMP/run.sh" <<EOF
mount --bind /usr/bin "$TMP/orig-bin" && mount --bind "$TMP/bin" /usr/bin || exit 70
mount --bind /usr/lib/x86_64-linux-gnu "$TMP/orig-lib" && mount --bind "$TMP/lib" /usr/lib/x86_64-linux-gnu || exit 70
mount --bind /usr/share "$TMP/orig-share" && mount --bind "$TMP/share" /usr/share || exit 70
/usr/bin/Xvfb ":$D" -screen 0 1280x900x24 -nolisten tcp >"$TMP/xvfb.log" 2>&1 &
XP=\$!
sleep 3
kill -0 \$XP 2>/dev/null || { echo "Xvfb did not start"; tail -5 "$TMP/xvfb.log"; exit 71; }
HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" XDG_DATA_HOME="$TMP/home/.local/share" DISPLAY=":$D" NO_AT_BRIDGE=1 \
  WEBKIT_DISABLE_COMPOSITING_MODE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1 GIO_USE_PROXY_RESOLVER=dummy \
  timeout 60 "$BIN" --smoke ${HUB:+--hub "$HUB"} > "$TMP/app.log" 2>&1
C=\$?
kill \$XP 2>/dev/null
exit \$C
EOF
unshare -rm sh "$TMP/run.sh"
CODE=$?
grep -E '^SMOKE' "$TMP/app.log" 2>/dev/null || tail -12 "$TMP/app.log" 2>/dev/null
rm -rf "$TMP"
exit $CODE
