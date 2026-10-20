#!/bin/sh
# Builds VibeRoller's native programs for Linux and Windows from this Linux machine (board T-122 / T-123 / T-134).
#
#   sh native/build.sh            test, then release builds for x86_64 Linux + Windows, copied to native/dist/<target>/
#   sh native/build.sh test       cargo test only
#   sh native/build.sh check      cargo check for both targets (fast, no binaries)
#
# Toolchains, all in user space (no root):
#   Rust     rustup in the hub account's home (RUSTUP_HOME / CARGO_HOME default to ~/.rustup, ~/.cargo; set them to
#            reach another account's install)
#   Linux    the system gcc as the linker
#   Windows  the x86_64-pc-windows-gnu target linked by mingw-w64: a system x86_64-w64-mingw32-gcc, else llvm-mingw
#            unpacked at ~/.local/opt/llvm-mingw (or LLVM_MINGW=<folder>)
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
OWNER_HOME=${VR_TOOLCHAIN_HOME:-$(getent passwd "$(id -un)" | cut -d: -f6)}
export RUSTUP_HOME=${RUSTUP_HOME:-$OWNER_HOME/.rustup}
export CARGO_HOME=${CARGO_HOME:-$OWNER_HOME/.cargo}
LLVM_MINGW=${LLVM_MINGW:-$OWNER_HOME/.local/opt/llvm-mingw}
PATH=$CARGO_HOME/bin:$PATH
[ -d "$LLVM_MINGW/bin" ] && PATH=$PATH:$LLVM_MINGW/bin
export PATH
command -v cargo >/dev/null || { echo "cargo not found (looked in $CARGO_HOME/bin): install rustup first" >&2; exit 1; }
WIN=x86_64-pc-windows-gnu LINUX=x86_64-unknown-linux-gnu
# llvm-mingw has no libgcc / libgcc_eh, which Rust's windows-gnu target links: its compiler-rt builtins and libunwind
# are the same things, so they stand in under those names (made once, next to its own libraries)
if [ -d "$LLVM_MINGW/x86_64-w64-mingw32/lib" ]; then
  L=$LLVM_MINGW/x86_64-w64-mingw32/lib
  [ -e "$L/libgcc_eh.a" ] || ln -s libunwind.a "$L/libgcc_eh.a"
  [ -e "$L/libgcc.a" ] || { B=$(find "$LLVM_MINGW/lib/clang" -name 'libclang_rt.builtins-x86_64.a' | head -1); [ -n "$B" ] && ln -s "$B" "$L/libgcc.a"; }
fi
if command -v x86_64-w64-mingw32-gcc >/dev/null; then
  export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
  command -v x86_64-w64-mingw32-ar >/dev/null && export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
  export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
  HAVE_WIN=1
else
  echo "no mingw-w64 linker (x86_64-w64-mingw32-gcc): Windows builds skipped" >&2; HAVE_WIN=0
fi
# What the binaries are told at build time: the project version + commit, and the public half of the release key the
# hub signs their updates with (bridge/release-key.json, made by the hub's native-releases.js on first use; made here
# the same way when the hub has not yet)
ROOT=$(cd "$HERE/.." && pwd)
if command -v node >/dev/null; then
  export VR_VERSION=${VR_VERSION:-$(node -e "console.log(require('$ROOT/versions.json').project)" 2>/dev/null)}
  export VR_RELEASE_PUBKEY=${VR_RELEASE_PUBKEY:-$(cd "$ROOT" && node -e "console.log(require('./bridge/native-releases')({ ROOT: process.env.VR_RELEASE_KEY_DIR || '$ROOT/bridge', MILLS_DIR: '$ROOT' }).publicKey())" 2>/dev/null)}
fi
export VR_COMMIT=${VR_COMMIT:-$(git -C "$ROOT" rev-parse HEAD 2>/dev/null)}
# the desktop app (Tauri) on Linux needs WebKitGTK's development files: a user-space sysroot (native/tauri-env.sh);
# without it the Linux build leaves the desktop app out (the Windows one needs nothing more)
export OWNER_HOME
. "$HERE/tauri-env.sh"
LINUX_SKIP=""; [ "$HAVE_TAURI_LINUX" = 1 ] || { LINUX_SKIP="--exclude vr-desktop"; echo "no WebKitGTK sysroot (sh native/tauri-env.sh setup): the Linux desktop app is skipped" >&2; }
cd "$HERE"
case "${1:-all}" in
  test) cargo test --workspace $LINUX_SKIP ;;
  check) cargo check --workspace $LINUX_SKIP --target "$LINUX"; [ "$HAVE_WIN" = 1 ] && cargo check --workspace --target "$WIN" ;;
  all)
    cargo test --workspace $LINUX_SKIP
    cargo build --workspace $LINUX_SKIP --release --target "$LINUX"
    [ "$HAVE_WIN" = 1 ] && cargo build --workspace --release --target "$WIN"
    # every binary of the workspace, per target, next to a SHA256SUMS the hub's Downloads can serve and check; made
    # beside the old set and swapped in at once, so the hub never hands out half a release
    for T in "$LINUX" "$WIN"; do
      [ -d "target/$T/release" ] || continue
      rm -rf "dist/.next-$T"; mkdir -p "dist/.next-$T"
      for b in vr-host vr-hostw vr-desktop; do for f in "target/$T/release/$b" "target/$T/release/$b.exe"; do [ -f "$f" ] && cp "$f" "dist/.next-$T/"; done; done
      printf '{"version":"%s","commit":"%s","target":"%s","builtAt":"%s"}\n' "${VR_VERSION:-}" "${VR_COMMIT:-}" "$T" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "dist/.next-$T/BUILD.json"
      (cd "dist/.next-$T" && for f in *; do case "$f" in SHA256SUMS|BUILD.json|'*') ;; *) sha256sum "$f";; esac; done > SHA256SUMS)
      rm -rf "dist/.old-$T"; [ -d "dist/$T" ] && mv "dist/$T" "dist/.old-$T"; mv "dist/.next-$T" "dist/$T"; rm -rf "dist/.old-$T"
      echo "dist/$T: $(ls "dist/$T" | grep -vcE '^(SHA256SUMS|BUILD.json)$') binaries"
    done ;;
  *) echo "usage: sh native/build.sh [all|test|check]" >&2; exit 2 ;;
esac
