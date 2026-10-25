#!/bin/sh
# Build localhost/pgm-re-base:<version> and :latest with podman (or docker). Linux / macOS / WSL.
#   ./build.sh            build
#   ./build.sh --test     build, then run smoke.sh against a fresh container
#   ENGINE=docker ./build.sh      choose the engine (default: podman, else docker)
#   ENGINE_ARGS="--root /x --runroot /y" ./build.sh   extra engine arguments (a different storage, say)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
version=$(tr -d ' \r\n' < "$here/VERSION")
name=localhost/pgm-re-base
if [ -z "${ENGINE:-}" ]; then
  if command -v podman >/dev/null 2>&1; then ENGINE=podman; elif command -v docker >/dev/null 2>&1; then ENGINE=docker; else echo "need podman or docker" >&2; exit 1; fi
fi
# shellcheck disable=SC2086
$ENGINE ${ENGINE_ARGS:-} build --file "$here/Containerfile" --build-arg "IMAGE_VERSION=$version" \
  --tag "$name:$version" --tag "$name:latest" "$here"
# shellcheck disable=SC2086
$ENGINE ${ENGINE_ARGS:-} images --format '{{.Repository}}:{{.Tag}} {{.Size}}' "$name"
if [ "${1:-}" = "--test" ]; then
  ENGINE=$ENGINE ENGINE_ARGS=${ENGINE_ARGS:-} "$here/smoke.sh" "$name:$version"
fi
