#!/bin/sh
# Sourced by native/build.sh: what building the Tauri desktop app (native/crates/vr-desktop) on Linux needs, without
# root. The WebKitGTK / GTK / libsoup / appindicator development files live in a user-space sysroot
# (~/.local/opt/tauri-sysroot), made once from Debian's own packages:
#   apt-cache depends --recurse … libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev
#     libayatana-appindicator3-dev libxdo-dev pkgconf  →  apt-get download  →  dpkg -x into the sysroot
# (sh native/tauri-env.sh setup does that). pkg-config answers from the sysroot; the linker finds the libraries there.
# The app itself runs against the WebKitGTK of the machine it runs on (libwebkit2gtk-4.1-0 on the desktop).
SYSROOT=${VR_TAURI_SYSROOT:-${OWNER_HOME:-$HOME}/.local/opt/tauri-sysroot}
if [ "${1:-}" = setup ]; then
  set -eu
  D=$(dirname "$SYSROOT")/tauri-debs; mkdir -p "$D" "$SYSROOT"; cd "$D"
  apt-cache depends --recurse --no-recommends --no-suggests --no-conflicts --no-breaks --no-replaces --no-enhances \
    libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev libxdo-dev pkgconf \
    | grep -E '^[a-z0-9]' | sort -u | xargs apt-get download -q
  for f in *.deb; do dpkg -x "$f" "$SYSROOT"; done
  echo "sysroot ready: $SYSROOT"; exit 0
fi
if [ -x "$SYSROOT/usr/bin/pkgconf" ]; then
  W=$(dirname "$SYSROOT")/tauri-bin; mkdir -p "$W"
  printf '#!/bin/sh\nLD_LIBRARY_PATH="%s/usr/lib/x86_64-linux-gnu" exec "%s/usr/bin/pkgconf" "$@"\n' "$SYSROOT" "$SYSROOT" > "$W/pkg-config"; chmod +x "$W/pkg-config"
  export PATH="$W:$PATH" PKG_CONFIG="$W/pkg-config" PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
  export PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/x86_64-linux-gnu/pkgconfig:$SYSROOT/usr/share/pkgconfig:$SYSROOT/usr/lib/pkgconfig"
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS:-} -L $SYSROOT/usr/lib/x86_64-linux-gnu -C link-arg=-Wl,-rpath-link,$SYSROOT/usr/lib/x86_64-linux-gnu -C link-arg=-Wl,-rpath-link,$SYSROOT/lib/x86_64-linux-gnu"
  export HAVE_TAURI_LINUX=1
else
  export HAVE_TAURI_LINUX=0
fi
