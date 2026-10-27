#!/bin/sh
# The PGM products' desktop apps (board T-150): the VibeRoller desktop app (native/crates/vr-desktop) built once per
# product, pointed at the product's address and carrying its name, for Windows (the Linux build needs the WebKitGTK
# sysroot, as VibeRoller's own). Each build is zipped with WebView2Loader.dll and a README and attached to the
# product's release on Forgejo (the tag its main is on), so people download it from the product's Releases page.
#
#   sh native/build-products.sh            build + publish every product
#   sh native/build-products.sh shadowchat build + publish one
#   PUBLISH=0 sh native/build-products.sh  build only (zips in native/dist/products/)
# Needs the toolchain native/build.sh uses (mingw-w64 for Windows) and, to publish, FORGEJO_URL + FORGEJO_TOKEN.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
OWNER_HOME=${VR_TOOLCHAIN_HOME:-$(getent passwd "$(id -un)" | cut -d: -f6)}
export RUSTUP_HOME=${RUSTUP_HOME:-$OWNER_HOME/.rustup} CARGO_HOME=${CARGO_HOME:-$OWNER_HOME/.cargo}
LLVM_MINGW=${LLVM_MINGW:-$OWNER_HOME/.local/opt/llvm-mingw}
PATH=$CARGO_HOME/bin:$PATH; [ -d "$LLVM_MINGW/bin" ] && PATH=$PATH:$LLVM_MINGW/bin; export PATH
command -v x86_64-w64-mingw32-gcc >/dev/null || { echo "no mingw-w64 (x86_64-w64-mingw32-gcc): cannot build the Windows apps" >&2; exit 1; }
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
command -v x86_64-w64-mingw32-ar >/dev/null && export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
WIN=x86_64-pc-windows-gnu LINUX=x86_64-unknown-linux-gnu
export OWNER_HOME
. "$HERE/tauri-env.sh"
HUB=${PRODUCTS_HUB:-https://portal.unityailab.com}
GIT=${PRODUCTS_GIT:-https://git.unityailab.com/UnityAILab}
cd "$HERE"
# Clean build law (LAWS.md): the builds go into a temporary target folder deleted when this script ends
TGT=$(mktemp -d "${VR_BUILD_TMP:-${TMPDIR:-/tmp}}/vr-products-XXXXXX")
trap 'rm -rf "$TGT"' EXIT INT TERM
# id | name | app identifier (installs beside VibeRoller's and each other's)
PRODUCTS='prodsuit|PGM Prod Suit|com.psygraphmedia.prodsuit
shadowchat|PGM ShaDow Chat|com.psygraphmedia.shadowchat
hyperprodman|PGM Hyper Prod Man|com.psygraphmedia.hyperprodman'
ONLY=${1:-}
mkdir -p dist/products
echo "$PRODUCTS" | while IFS='|' read -r id name ident; do
  [ -n "$ONLY" ] && [ "$ONLY" != "$id" ] && continue
  # the product's version: the tag its main is on (Forgejo), else "dev"
  tag=$(git ls-remote --tags "$GIT/pgm-$id.git" 2>/dev/null | sed -n 's#.*refs/tags/\(v[0-9.]*\)$#\1#p' | sort -V | tail -n 1); tag=${tag:-dev}
  echo "== $name ($id, $tag): building the Windows app"
  # the same program as VibeRoller's app, with the product's address, name and identity built in (TAURI_CONFIG: a
  # patch over tauri.conf.json; VR_DEFAULT_HUB / VR_APP_NAME: vr-desktop's settings and window title)
  VR_DEFAULT_HUB="$HUB/p/$id" VR_APP_NAME="$name" VR_VERSION="${tag#v}" \
    TAURI_CONFIG="{\"productName\":\"$name\",\"identifier\":\"$ident\"}" \
    nice -n 10 cargo build -p vr-desktop --release --target "$WIN" --target-dir "$TGT" -q
  out="dist/products/$id-windows-$tag.zip"; stage="dist/products/.stage-$id"; rm -rf "$stage"; mkdir -p "$stage"
  cp "$TGT/$WIN/release/vr-desktop.exe" "$stage/$name.exe"
  cp "$TGT/$WIN/release/WebView2Loader.dll" "$stage/"
  printf '%s\n' "$name, the desktop app (Windows), $tag" "" "Unzip anywhere and run \"$name.exe\". It opens $HUB/p/$id/ in its own window." "Sign in with your VibeRoller username and password; the first time, your $name account is made from it." "" "The app is VibeRoller's desktop app with this product's address and name built in (https://git.unityailab.com/UnityAILab/VibeRoller, native/crates/vr-desktop)." > "$stage/README.txt"
  rm -f "$out"; node "$HERE/zip.js" "$out" "$stage" >/dev/null
  rm -rf "$stage"
  echo "   $out ($(du -h "$out" | cut -f1))"
  # Linux too, when the WebKitGTK sysroot is there (native/tauri-env.sh setup; the app runs against the desktop's own
  # libwebkit2gtk-4.1): a tar.gz with the binary and the README
  outl=""
  if [ "$HAVE_TAURI_LINUX" = 1 ]; then
    echo "   building the Linux app"
    VR_DEFAULT_HUB="$HUB/p/$id" VR_APP_NAME="$name" VR_VERSION="${tag#v}" \
      TAURI_CONFIG="{\"productName\":\"$name\",\"identifier\":\"$ident\"}" \
      nice -n 10 cargo build -p vr-desktop --release --target "$LINUX" --target-dir "$TGT" -q
    outl="dist/products/$id-linux-$tag.tar.gz"; stage="dist/products/.stage-$id"; rm -rf "$stage"; mkdir -p "$stage"
    cp "$TGT/$LINUX/release/vr-desktop" "$stage/$id"
    printf '%s\n' "$name, the desktop app (Linux x86_64), $tag" "" "Unpack anywhere and run ./$id (it needs the desktop's WebKitGTK: libwebkit2gtk-4.1-0, as VibeRoller's own app). It opens $HUB/p/$id/ in its own window." "Sign in with your VibeRoller username and password; the first time, your $name account is made from it." > "$stage/README.txt"
    rm -f "$outl"; tar -czf "$outl" -C "$stage" .
    rm -rf "$stage"
    echo "   $outl ($(du -h "$outl" | cut -f1))"
  fi
  [ "${PUBLISH:-1}" = 1 ] || continue
  [ "$tag" = dev ] && { echo "   no tag on Forgejo: not published"; continue; }
  : "${FORGEJO_URL:?FORGEJO_URL is needed to publish}" "${FORGEJO_TOKEN:?FORGEJO_TOKEN is needed to publish}"
  api="$FORGEJO_URL/api/v1/repos/UnityAILab/pgm-$id"
  auth="Authorization: token $FORGEJO_TOKEN"
  # the release for that tag (made if missing), then the zip as its asset (an older one of the same name replaced)
  rel=$(curl -fsS -m 20 -H "$auth" "$api/releases/tags/$tag" 2>/dev/null || true)
  if [ -z "$rel" ]; then
    rel=$(curl -fsS -m 20 -H "$auth" -H 'Content-Type: application/json' -X POST "$api/releases" -d "{\"tag_name\":\"$tag\",\"name\":\"$name $tag\",\"body\":\"$name $tag. The Windows desktop app is attached; it opens $HUB/p/$id/ in its own window. Linux and the web: $HUB/p/$id/\",\"draft\":false,\"prerelease\":false}")
  fi
  rid=$(printf '%s' "$rel" | sed -n 's/^{"id":\([0-9]*\),.*/\1/p'); [ -n "$rid" ] || rid=$(printf '%s' "$rel" | tr ',' '\n' | sed -n 's/^{\{0,1\}"id":\([0-9]*\)$/\1/p' | head -n 1)
  [ -n "$rid" ] || { echo "   could not read the release id: $rel" >&2; exit 1; }
  for f in "$out" $outl; do
    zname=$(basename "$f")
    old=$(curl -fsS -m 20 -H "$auth" "$api/releases/$rid/assets" | tr '{' '\n' | grep "\"name\":\"$zname\"" | sed -n 's/.*"id":\([0-9]*\),.*/\1/p' | head -n 1 || true)
    [ -n "$old" ] && curl -fsS -m 20 -H "$auth" -X DELETE "$api/releases/$rid/assets/$old" >/dev/null
    curl -fsS -m 300 -H "$auth" -X POST "$api/releases/$rid/assets?name=$zname" -F "attachment=@$f" >/dev/null
  done
  echo "   published: $GIT/pgm-$id/releases/tag/$tag"
done
