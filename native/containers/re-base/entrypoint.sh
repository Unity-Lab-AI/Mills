#!/bin/sh
# pgm-re-base entrypoint: print what is inside, then run the command (default: sleep infinity).
# Set PGM_RE_QUIET=1 to skip the banner. Nothing is downloaded or changed here.
if [ -z "$PGM_RE_QUIET" ]; then
  v() { printf '  %-12s %s\n' "$1" "$2"; }
  first() { "$@" 2>&1 | head -n 1; }
  echo "pgm-re-base ${PGM_RE_BASE_VERSION:-?} (user $(id -un), exchange: /work/in /work/out, scratch: /work/tmp)"
  v node "$(node --version 2>&1)"
  v rea-agents "$(node -p "require('/opt/rea/node_modules/rea-agents/package.json').version" 2>&1)"
  v ghidra "$(sed -n 's/^application.version=//p' "$GHIDRA_INSTALL_DIR/Ghidra/application.properties" 2>/dev/null)"
  v jdk "$(first java -version)"
  v frida "$(first frida --version)"
  v lldb "$(first lldb --version)"
  v gdb "$(first gdb --version)"
  v adb "$(first adb version)"
  v radare2 "$(first r2 -v)"
  v python3 "$(first python3 --version)"
  v skill "/opt/rea/skill/reverse-engineer-anything"
fi
exec "$@"
