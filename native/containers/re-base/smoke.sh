#!/bin/sh
# Smoke test for pgm-re-base. Two ways to run it:
#   ./smoke.sh [image]     from the host: starts a throwaway container (no network), runs the checks in it, removes it.
#   /opt/re-smoke.sh --inside      inside a running container (this is what the host mode calls; also what build.cmd uses).
# ENGINE (podman|docker) and ENGINE_ARGS are the same as in build.sh. Exit 0 = all checks passed.
set -u

if [ "${1:-}" != "--inside" ]; then
  image=${1:-localhost/pgm-re-base:latest}
  if [ -z "${ENGINE:-}" ]; then
    if command -v podman >/dev/null 2>&1; then ENGINE=podman; else ENGINE=docker; fi
  fi
  name=pgm-re-smoke-$$
  # shellcheck disable=SC2086
  $ENGINE ${ENGINE_ARGS:-} run -d --name "$name" --network none "$image" >/dev/null || exit 2
  # shellcheck disable=SC2086
  $ENGINE ${ENGINE_ARGS:-} exec -e PGM_RE_QUIET=1 "$name" /opt/re-smoke.sh --inside
  rc=$?
  # shellcheck disable=SC2086
  $ENGINE ${ENGINE_ARGS:-} rm -f -t 0 "$name" >/dev/null 2>&1
  exit $rc
fi

# ---------------------------------------------------------------- inside the container
pass=0; fail=0
ok()  { pass=$((pass + 1)); printf 'PASS  %s\n' "$*"; }
bad() { fail=$((fail + 1)); printf 'FAIL  %s\n' "$*"; }
check() { # check "<label>" <command...>: the command must exit 0
  label=$1; shift
  if out=$("$@" 2>&1); then ok "$label"; else bad "$label: $(printf '%s' "$out" | head -n 3 | tr '\n' ' ')"; fi
}
T=$(mktemp -d /work/tmp/smoke.XXXXXX) || { echo "cannot make a scratch folder in /work/tmp"; exit 2; }
trap 'rm -rf "$T"' EXIT

want_rea=$(node -p "require('/opt/rea/node_modules/rea-agents/package.json').version")

# 1. REA CLI
v=$(rea --version 2>&1 | head -n 1)
case "$v" in *"$want_rea"*) ok "rea --version: $v" ;; *) bad "rea --version: '$v' (wanted $want_rea)" ;; esac

# 2. REA MCP server: initialize + tools/list over stdio
init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pgm-re-smoke","version":"1"}}}'
done_='{"jsonrpc":"2.0","method":"notifications/initialized"}'
list='{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
(printf '%s\n%s\n%s\n' "$init" "$done_" "$list"; sleep 10) | rea mcp > "$T/mcp.out" 2> "$T/mcp.err"
count=$(jq -s '[.[] | select(.id == 2) | .result.tools | length] | max // 0' "$T/mcp.out" 2>/dev/null || echo 0)
srv=$(jq -rs '[.[] | select(.id == 1) | .result.serverInfo.name + " " + .result.serverInfo.version] | first // ""' "$T/mcp.out" 2>/dev/null)
if [ "${count:-0}" -ge 100 ]; then ok "rea mcp: initialize ($srv) + tools/list = $count tools"; else bad "rea mcp: tools/list gave '$count' tools; $(head -c 300 "$T/mcp.err")"; fi

# 3. Static analysis of a tiny JavaScript / Electron-style app
rea analyze-javascript-application /opt/re-fixtures/js-app --format json > "$T/js.out" 2> "$T/js.err"
if grep -q 'main.js' "$T/js.out" && grep -q 'api.example.invalid' "$T/js.out"; then
  ok "rea analyze-javascript-application: sample app analysed, found main.js and its endpoint"
else bad "rea analyze-javascript-application: expected markers missing; $(head -c 300 "$T/js.err")"; fi

# 4. Ghidra headless on a tiny compiled C binary, and REA's own view of Ghidra
mkdir -p "$T/gproj"
if /opt/ghidra/support/analyzeHeadless "$T/gproj" smoke -import /opt/re-fixtures/hello -overwrite > "$T/ghidra.log" 2>&1 \
   && grep -q 'Import succeeded' "$T/ghidra.log" && grep -q 'Analysis succeeded' "$T/ghidra.log"; then
  ok "ghidra analyzeHeadless: import + analysis of /opt/re-fixtures/hello succeeded"
else bad "ghidra analyzeHeadless: $(tail -n 5 "$T/ghidra.log" | tr '\n' ' ')"; fi
if rea doctor --provider ghidra --format json 2>/dev/null | jq -e '.scope_checks | all(.ok)' >/dev/null; then ok "rea doctor --provider ghidra: every Ghidra check healthy"; else bad "rea doctor --provider ghidra"; fi

# 5. Runtime tools
for spec in "frida:frida --version" "lldb:lldb --version" "adb:adb version" "gdb:gdb --version" "radare2:r2 -v" \
            "java:java -version" "strace:strace -V" "ltrace:ltrace --version" "objdump:objdump --version" "file:file --version" \
            "strings:strings --version" "jq:jq --version" "7z:7z i" "unzip:unzip -v" "python3:python3 --version"; do
  label=${spec%%:*}; cmd=${spec#*:}
  # shellcheck disable=SC2086
  o=$($cmd 2>&1 | head -n 1) && ok "$label: $o" || bad "$label: $o"
done
check "frida python module" /opt/frida-venv/bin/python -c "import frida; print(frida.__version__)"

# 6. Layout, user, skill
[ "$(id -u)" = 1000 ] && ok "runs as uid 1000 ($(id -un))" || bad "uid is $(id -u), wanted 1000"
for d in /work/in /work/out /work/tmp; do [ -w "$d" ] && ok "$d is writable" || bad "$d is not writable"; done
[ -s /opt/rea/skill/reverse-engineer-anything/SKILL.md ] && [ -d /opt/rea/skill/reverse-engineer-anything/references ] \
  && ok "REA skill at /opt/rea/skill/reverse-engineer-anything ($(find /opt/rea/skill -type f | wc -l) files)" || bad "REA skill missing"
[ -z "$(find /home/re -maxdepth 2 \( -name '.claude*' -o -name '.codex' -o -name '.agents' \) 2>/dev/null)" ] \
  && ok "no agent config written (rea setup was never run)" || bad "agent config found in /home/re"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
