#!/bin/sh
# A tiny fake MCP stdio server (newline-delimited JSON-RPC) for tests: initialize, tools/list, tools/call. Plain sh + sed,
# so any image has it. Tools: echo (returns its arguments and this server's pid), big (100000 x), nope (an isError result).
# Anything else: a JSON-RPC error. It expects the client's key order: {"jsonrpc":"2.0","id":N,"method":...}.
esc() { sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"jsonrpc":"2.0","id":\([0-9][0-9]*\),.*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake-rea","version":"0.0.1"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo the arguments back.\\nSecond line.","inputSchema":{"type":"object"}},{"name":"big","description":"A result of 100000 characters.","inputSchema":{"type":"object"}},{"name":"nope","description":"Always fails.","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      tool=$(printf '%s' "$line" | sed -n 's/^.*"params":{"name":"\([^"]*\)".*/\1/p')
      case "$tool" in
        echo)
          args=$(printf '%s' "$line" | sed -n 's/^.*"arguments":\(.*\)}}$/\1/p' | esc)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"[untrusted content] pid=%s args=%s"}]}}\n' "$id" "$$" "$args" ;;
        big)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"[untrusted content] %s"}]}}\n' "$id" "$(head -c 100000 /dev/zero | tr '\0' x)" ;;
        nope)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"isError":true,"content":[{"type":"text","text":"it failed"}]}}\n' "$id" ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"Unknown tool: %s"}}\n' "$id" "$tool" ;;
      esac ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
  esac
done
